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
        return Err(KoraError::Unauthorized);
    }
    env.storage().instance().set(&GuardKey::ReentrancyGuard, &true);
    Ok(())
}

/// Release the reentrancy guard. Must be called before returning from protected functions.
pub fn release_guard(env: &Env) {
    env.storage().instance().remove(&GuardKey::ReentrancyGuard);
}

/// RAII-style guard that automatically releases on drop.
pub struct ReentrancyGuard<'a> {
    env: &'a Env,
}

impl<'a> ReentrancyGuard<'a> {
    pub fn new(env: &'a Env) -> Result<Self, KoraError> {
        acquire_guard(env)?;
        Ok(ReentrancyGuard { env })
    }
}

impl<'a> Drop for ReentrancyGuard<'a> {
    fn drop(&mut self) {
        release_guard(self.env);
    }
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
    fn test_raii_guard() {
        let env = Env::default();
        {
            let _guard = ReentrancyGuard::new(&env).unwrap();
            assert!(ReentrancyGuard::new(&env).is_err());
        }
        assert!(ReentrancyGuard::new(&env).is_ok());
    }
}
