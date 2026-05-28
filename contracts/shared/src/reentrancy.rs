use soroban_sdk::{contracttype, Env};
use crate::errors::KoraError;

/// Reentrancy guard using a simple flag-based approach.
/// Prevents recursive calls within the same transaction.
#[contracttype]
pub enum GuardKey {
    ReentrancyGuard,
}

/// Acquire a reentrancy guard. Must be called at the start of protected functions.
pub fn acquire_guard(env: &Env) -> Result<(), KoraError> {
    if env.storage().instance().has(&GuardKey::ReentrancyGuard) {
        return Err(KoraError::Reentrancy);
    }
    env.storage().instance().set(&GuardKey::ReentrancyGuard, &true);
    Ok(())
}

/// Release the reentrancy guard. Must be called before returning from protected functions.
pub fn release_guard(env: &Env) {
    env.storage().instance().remove(&GuardKey::ReentrancyGuard);
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn test_guard_acquire_release() {
        let env = Env::default();
        assert!(acquire_guard(&env).is_ok());
        assert!(acquire_guard(&env).is_err());
        release_guard(&env);
        assert!(acquire_guard(&env).is_ok());
    }

    #[test]
    fn test_double_acquire_returns_reentrancy_error() {
        let env = Env::default();
        acquire_guard(&env).unwrap();
        let err = acquire_guard(&env).unwrap_err();
        assert_eq!(err, KoraError::Reentrancy);
        release_guard(&env);
    }

    #[test]
    fn test_release_without_acquire_is_safe() {
        let env = Env::default();
        // Should not panic
        release_guard(&env);
        assert!(acquire_guard(&env).is_ok());
        release_guard(&env);
    }
}
