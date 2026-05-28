#![no_std]

use kora_shared::{
    errors::KoraError,
    types::SmeProfile,
    validation::{require_non_empty_bytes, require_valid_risk_score},
};
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Bytes, Env};

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    Verifier(Address),
    SmeProfile(Address),
    DebtorScore(Bytes), // keyed by debtor_hash
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct RiskRegistryContract;

#[contractimpl]
impl RiskRegistryContract {
    /// Initialise the contract. Can only be called once.
    pub fn initialize(env: Env, admin: Address) -> Result<(), KoraError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(KoraError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    // ── Admin management ──────────────────────────────────────────────────────

    /// Transfer admin rights to a new address. Current admin must sign.
    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), KoraError> {
        current_admin.require_auth();
        Self::require_admin(&env, &current_admin)?;
        if current_admin == new_admin {
            return Err(KoraError::InvalidAddress);
        }
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.events().publish(
            (symbol_short!("ADM_TRF"),),
            (current_admin, new_admin),
        );
        Ok(())
    }

    // ── Verifier management ───────────────────────────────────────────────────

    /// Admin adds a trusted verifier.
    pub fn add_verifier(env: Env, admin: Address, verifier: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage()
            .persistent()
            .set(&DataKey::Verifier(verifier.clone()), &true);
        env.events().publish(
            (symbol_short!("VRF_ADD"),),
            verifier,
        );
        Ok(())
    }

    /// Admin removes a verifier.
    pub fn remove_verifier(env: Env, admin: Address, verifier: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        // Remove the key entirely rather than setting to false to keep storage clean
        env.storage()
            .persistent()
            .remove(&DataKey::Verifier(verifier.clone()));
        env.events().publish(
            (symbol_short!("VRF_REM"),),
            verifier,
        );
        Ok(())
    }

    // ── SME management ────────────────────────────────────────────────────────

    /// Verifier registers and scores an SME.
    ///
    /// Rejects re-registration of an already-verified SME to prevent
    /// accidental overwrites. Use `update_sme_score` to change the score.
    pub fn register_sme(
        env: Env,
        verifier: Address,
        sme: Address,
        risk_score: u32,
    ) -> Result<(), KoraError> {
        verifier.require_auth();
        Self::require_verifier(&env, &verifier)?;
        require_valid_risk_score(risk_score)?;

        // Prevent silent overwrite of an existing verified profile
        if env
            .storage()
            .persistent()
            .has(&DataKey::SmeProfile(sme.clone()))
        {
            return Err(KoraError::AlreadyInitialized);
        }

        let profile = SmeProfile {
            address: sme.clone(),
            verified: true,
            verifier: verifier.clone(),
            risk_score,
            total_invoices: 0,
            defaults: 0,
            registered_at: env.ledger().timestamp(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::SmeProfile(sme.clone()), &profile);

        env.events().publish(
            (symbol_short!("SME_REG"),),
            (sme, verifier, risk_score),
        );
        Ok(())
    }

    /// Update SME risk score. Verifier only.
    pub fn update_sme_score(
        env: Env,
        verifier: Address,
        sme: Address,
        new_score: u32,
    ) -> Result<(), KoraError> {
        verifier.require_auth();
        Self::require_verifier(&env, &verifier)?;
        require_valid_risk_score(new_score)?;

        let mut profile: SmeProfile = env
            .storage()
            .persistent()
            .get(&DataKey::SmeProfile(sme.clone()))
            .ok_or(KoraError::SMENotRegistered)?;

        let old_score = profile.risk_score;
        profile.risk_score = new_score;
        env.storage()
            .persistent()
            .set(&DataKey::SmeProfile(sme.clone()), &profile);

        env.events().publish(
            (symbol_short!("SME_UPD"),),
            (sme, old_score, new_score),
        );
        Ok(())
    }

    /// Increment invoice count for an SME. Called by the Invoice NFT contract.
    pub fn increment_invoice_count(
        env: Env,
        caller: Address,
        sme: Address,
    ) -> Result<(), KoraError> {
        caller.require_auth();

        let mut profile: SmeProfile = env
            .storage()
            .persistent()
            .get(&DataKey::SmeProfile(sme.clone()))
            .ok_or(KoraError::SMENotRegistered)?;

        profile.total_invoices = profile
            .total_invoices
            .checked_add(1)
            .ok_or(KoraError::ArithmeticOverflow)?;

        env.storage()
            .persistent()
            .set(&DataKey::SmeProfile(sme), &profile);
        Ok(())
    }

    /// Record a default against an SME. Admin only.
    pub fn record_default(env: Env, admin: Address, sme: Address) -> Result<(), KoraError> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        let mut profile: SmeProfile = env
            .storage()
            .persistent()
            .get(&DataKey::SmeProfile(sme.clone()))
            .ok_or(KoraError::SMENotRegistered)?;

        profile.defaults = profile
            .defaults
            .checked_add(1)
            .ok_or(KoraError::ArithmeticOverflow)?;

        env.storage()
            .persistent()
            .set(&DataKey::SmeProfile(sme.clone()), &profile);

        env.events().publish(
            (symbol_short!("SME_DFT"),),
            (sme, profile.defaults),
        );
        Ok(())
    }

    // ── Debtor scores ─────────────────────────────────────────────────────────

    /// Store a debtor risk score keyed by debtor hash. Verifier only.
    ///
    /// `debtor_hash` must be non-empty (expected to be a 32-byte SHA-256 digest).
    pub fn set_debtor_score(
        env: Env,
        verifier: Address,
        debtor_hash: Bytes,
        score: u32,
    ) -> Result<(), KoraError> {
        verifier.require_auth();
        Self::require_verifier(&env, &verifier)?;
        require_non_empty_bytes(&debtor_hash)?;
        require_valid_risk_score(score)?;

        env.storage()
            .persistent()
            .set(&DataKey::DebtorScore(debtor_hash.clone()), &score);

        env.events().publish(
            (symbol_short!("DBT_SCR"),),
            (debtor_hash, score),
        );
        Ok(())
    }

    // ── Views ─────────────────────────────────────────────────────────────────

    pub fn get_sme_profile(env: Env, sme: Address) -> Result<SmeProfile, KoraError> {
        env.storage()
            .persistent()
            .get(&DataKey::SmeProfile(sme))
            .ok_or(KoraError::SMENotRegistered)
    }

    pub fn is_verified_sme(env: Env, sme: Address) -> bool {
        env.storage()
            .persistent()
            .get::<DataKey, SmeProfile>(&DataKey::SmeProfile(sme))
            .map(|p| p.verified)
            .unwrap_or(false)
    }

    pub fn is_verifier(env: Env, verifier: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Verifier(verifier))
            .unwrap_or(false)
    }

    pub fn get_debtor_score(env: Env, debtor_hash: Bytes) -> Option<u32> {
        env.storage()
            .persistent()
            .get(&DataKey::DebtorScore(debtor_hash))
    }

    pub fn get_admin(env: Env) -> Result<Address, KoraError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(KoraError::NotInitialized)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Address) -> Result<(), KoraError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(KoraError::NotInitialized)?;
        if &admin != caller {
            return Err(KoraError::NotAdmin);
        }
        Ok(())
    }

    fn require_verifier(env: &Env, caller: &Address) -> Result<(), KoraError> {
        let ok: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Verifier(caller.clone()))
            .unwrap_or(false);
        if !ok {
            return Err(KoraError::NotVerifier);
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Bytes, Env};

    fn setup() -> (Env, Address, RiskRegistryContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, RiskRegistryContract);
        let client = RiskRegistryContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, admin, client)
    }

    #[test]
    fn test_initialize_success() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, RiskRegistryContract);
        let client = RiskRegistryContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        assert!(client.try_initialize(&admin).is_ok());
    }

    #[test]
    fn test_initialize_already_initialized() {
        let (env, admin, client) = setup();
        assert!(client.try_initialize(&admin).is_err());
    }

    #[test]
    fn test_add_verifier_success() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        assert!(client.try_add_verifier(&admin, &verifier).is_ok());
        assert!(client.is_verifier(&verifier));
    }

    #[test]
    fn test_add_verifier_not_admin() {
        let (env, _admin, client) = setup();
        let verifier = Address::generate(&env);
        let stranger = Address::generate(&env);
        assert!(client.try_add_verifier(&stranger, &verifier).is_err());
    }

    #[test]
    fn test_remove_verifier_success() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        client.add_verifier(&admin, &verifier);
        assert!(client.is_verifier(&verifier));
        client.remove_verifier(&admin, &verifier);
        assert!(!client.is_verifier(&verifier));
    }

    #[test]
    fn test_register_sme_flow() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);

        client.add_verifier(&admin, &verifier);
        client.register_sme(&verifier, &sme, &35u32);
        assert!(client.is_verified_sme(&sme));

        let profile = client.get_sme_profile(&sme);
        assert_eq!(profile.risk_score, 35);
        assert_eq!(profile.defaults, 0);
        assert_eq!(profile.total_invoices, 0);
        assert!(profile.verified);
    }

    #[test]
    fn test_register_sme_duplicate_rejected() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);

        client.add_verifier(&admin, &verifier);
        client.register_sme(&verifier, &sme, &35u32);
        // Second registration of the same SME must fail
        assert!(client.try_register_sme(&verifier, &sme, &50u32).is_err());
    }

    #[test]
    fn test_register_sme_unverified_verifier() {
        let (env, _admin, client) = setup();
        let stranger = Address::generate(&env);
        let sme = Address::generate(&env);
        assert!(client.try_register_sme(&stranger, &sme, &10u32).is_err());
    }

    #[test]
    fn test_register_sme_invalid_risk_score() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);
        client.add_verifier(&admin, &verifier);
        assert!(client.try_register_sme(&verifier, &sme, &101u32).is_err());
    }

    #[test]
    fn test_update_sme_score_success() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);

        client.add_verifier(&admin, &verifier);
        client.register_sme(&verifier, &sme, &35u32);
        client.update_sme_score(&verifier, &sme, &50u32);
        assert_eq!(client.get_sme_profile(&sme).risk_score, 50);
    }

    #[test]
    fn test_update_sme_score_not_registered() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);
        client.add_verifier(&admin, &verifier);
        assert!(client.try_update_sme_score(&verifier, &sme, &50u32).is_err());
    }

    #[test]
    fn test_increment_invoice_count() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);
        let caller = Address::generate(&env);

        client.add_verifier(&admin, &verifier);
        client.register_sme(&verifier, &sme, &35u32);
        assert_eq!(client.get_sme_profile(&sme).total_invoices, 0);
        client.increment_invoice_count(&caller, &sme);
        assert_eq!(client.get_sme_profile(&sme).total_invoices, 1);
    }

    #[test]
    fn test_increment_invoice_count_multiple() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);
        let caller = Address::generate(&env);

        client.add_verifier(&admin, &verifier);
        client.register_sme(&verifier, &sme, &35u32);

        for i in 1..=5u32 {
            client.increment_invoice_count(&caller, &sme);
            assert_eq!(client.get_sme_profile(&sme).total_invoices, i);
        }
    }

    #[test]
    fn test_record_default() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);

        client.add_verifier(&admin, &verifier);
        client.register_sme(&verifier, &sme, &35u32);
        assert_eq!(client.get_sme_profile(&sme).defaults, 0);
        client.record_default(&admin, &sme);
        assert_eq!(client.get_sme_profile(&sme).defaults, 1);
    }

    #[test]
    fn test_record_default_not_admin() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let sme = Address::generate(&env);
        let stranger = Address::generate(&env);

        client.add_verifier(&admin, &verifier);
        client.register_sme(&verifier, &sme, &35u32);
        assert!(client.try_record_default(&stranger, &sme).is_err());
    }

    #[test]
    fn test_set_debtor_score() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[0xABu8; 32]);

        client.add_verifier(&admin, &verifier);
        client.set_debtor_score(&verifier, &debtor_hash, &45u32);
        assert_eq!(client.get_debtor_score(&debtor_hash), Some(45u32));
    }

    #[test]
    fn test_set_debtor_score_empty_hash_rejected() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let empty_hash = Bytes::new(&env);

        client.add_verifier(&admin, &verifier);
        assert!(client.try_set_debtor_score(&verifier, &empty_hash, &45u32).is_err());
    }

    #[test]
    fn test_set_debtor_score_invalid_score() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        let debtor_hash = Bytes::from_slice(&env, &[0xABu8; 32]);

        client.add_verifier(&admin, &verifier);
        assert!(client.try_set_debtor_score(&verifier, &debtor_hash, &101u32).is_err());
    }

    #[test]
    fn test_get_sme_profile_not_found() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        assert!(client.try_get_sme_profile(&sme).is_err());
    }

    #[test]
    fn test_is_verified_sme_false_for_unregistered() {
        let (env, _admin, client) = setup();
        let sme = Address::generate(&env);
        assert!(!client.is_verified_sme(&sme));
    }

    #[test]
    fn test_get_debtor_score_not_found() {
        let (env, _admin, client) = setup();
        let debtor_hash = Bytes::from_slice(&env, &[0xCDu8; 32]);
        assert_eq!(client.get_debtor_score(&debtor_hash), None);
    }

    #[test]
    fn test_risk_score_boundary_values() {
        let (env, admin, client) = setup();
        let verifier = Address::generate(&env);
        client.add_verifier(&admin, &verifier);

        let sme1 = Address::generate(&env);
        client.register_sme(&verifier, &sme1, &0u32);
        assert_eq!(client.get_sme_profile(&sme1).risk_score, 0);

        let sme2 = Address::generate(&env);
        client.register_sme(&verifier, &sme2, &100u32);
        assert_eq!(client.get_sme_profile(&sme2).risk_score, 100);

        let sme3 = Address::generate(&env);
        assert!(client.try_register_sme(&verifier, &sme3, &101u32).is_err());
    }

    #[test]
    fn test_multiple_verifiers() {
        let (env, admin, client) = setup();
        let verifier1 = Address::generate(&env);
        let verifier2 = Address::generate(&env);
        let sme1 = Address::generate(&env);
        let sme2 = Address::generate(&env);

        client.add_verifier(&admin, &verifier1);
        client.add_verifier(&admin, &verifier2);
        client.register_sme(&verifier1, &sme1, &30u32);
        client.register_sme(&verifier2, &sme2, &60u32);

        assert_eq!(client.get_sme_profile(&sme1).risk_score, 30);
        assert_eq!(client.get_sme_profile(&sme2).risk_score, 60);
        assert_eq!(client.get_sme_profile(&sme1).verifier, verifier1);
        assert_eq!(client.get_sme_profile(&sme2).verifier, verifier2);
    }

    #[test]
    fn test_transfer_admin_success() {
        let (env, admin, client) = setup();
        let new_admin = Address::generate(&env);

        client.transfer_admin(&admin, &new_admin);
        assert_eq!(client.get_admin(), new_admin);
    }

    #[test]
    fn test_transfer_admin_same_address_rejected() {
        let (_, admin, client) = setup();
        assert!(client.try_transfer_admin(&admin, &admin).is_err());
    }

    #[test]
    fn test_transfer_admin_non_admin_rejected() {
        let (env, _admin, client) = setup();
        let stranger = Address::generate(&env);
        let new_admin = Address::generate(&env);
        assert!(client.try_transfer_admin(&stranger, &new_admin).is_err());
    }
}
