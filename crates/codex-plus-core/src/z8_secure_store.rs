//! Platform credential-store persistence for Z8 account material.
//!
//! The manager keeps live credentials in memory, while this module stores the
//! restart payload in the Windows Credential Manager or macOS Keychain. Linux
//! intentionally returns an explicit unsupported error until a Secret Service
//! implementation is reviewed and enabled. No fallback to a plaintext file,
//! environment variable, or command-line argument is permitted.

use crate::z8_account::{AccountApiKey, AccountSession};
use serde_json::{json, Value};
use thiserror::Error;

pub const Z8_CREDENTIAL_SERVICE: &str = "com.z8.codex.manager";
// Credential Manager identifies generic entries by target alone; Keychain
// identifies them by service and account. Keep every new entry in a versioned
// Z8 namespace and never inspect or delete the unowned development targets.
const Z8_CREDENTIAL_NAMESPACE: &str = "com.z8.codex.manager:v3";
pub const Z8_CREDENTIAL_ACCOUNT: &str = "com.z8.codex.manager:v3:account";
const Z8_CREDENTIAL_FORMAT_VERSION: u64 = 2;
// A failed native-store deletion must not make the manager resurrect the
// account on the next status call.  Keep a small tombstone beside the account
// record until a subsequent login successfully replaces it.
const Z8_LOGOUT_TOMBSTONE_TARGET: &str = "com.z8.codex.manager:v3:logout-pending";
const Z8_LOGOUT_TOMBSTONE_PAYLOAD: &str = r#"{"version":1}"#;

/// `CREDENTIALW::CredentialBlobSize` is limited to
/// `CRED_MAX_CREDENTIAL_BLOB_SIZE` (5 * 512 bytes). All serialized account and
/// key records are checked before the first native write.
#[cfg(windows)]
pub const MAX_CREDENTIAL_BLOB_BYTES: usize = 5 * 512;
/// Retain the existing Keychain bound on macOS. Linux has no credential
/// backend yet but keeps the same parsing bound.
#[cfg(not(windows))]
pub const MAX_CREDENTIAL_BLOB_BYTES: usize = 5 * 1024;
/// The account API currently returns at most 1000 keys per page. Keep the same
/// bound on persisted bank indexes so a corrupt record cannot trigger an
/// unbounded cleanup or load loop.
const MAX_PERSISTED_KEYS: usize = 1000;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SecureStoreError {
    #[error("Z8 安全凭据存储不可用（{operation}）")]
    Backend { operation: &'static str },
    #[error("Z8 安全凭据内容无效")]
    InvalidPayload,
    #[error("Z8 安全凭据序列化失败")]
    Serialization,
}

/// A recovered account and its last successfully fetched API Key summaries.
/// The type intentionally has no `Debug` implementation: it contains secrets.
#[derive(Clone)]
pub struct PersistedAccount {
    pub session: AccountSession,
    pub keys: Vec<AccountApiKey>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Z8SecureStore;

impl Z8SecureStore {
    pub const fn new() -> Self {
        Self
    }

    /// Save the current account snapshot to the OS credential store.
    pub fn save(
        &self,
        session: &AccountSession,
        keys: &[AccountApiKey],
    ) -> Result<(), SecureStoreError> {
        self.save_with(
            session,
            keys,
            platform::load,
            platform::save,
            platform::clear,
        )
    }

    fn save_with<L, S, C>(
        &self,
        session: &AccountSession,
        keys: &[AccountApiKey],
        mut load: L,
        save: S,
        mut clear: C,
    ) -> Result<(), SecureStoreError>
    where
        L: FnMut(&str) -> Result<Option<String>, SecureStoreError>,
        S: FnMut(&str, &str) -> Result<(), SecureStoreError>,
        C: FnMut(&str) -> Result<(), SecureStoreError>,
    {
        // Alternate banks so a failed write cannot corrupt the currently
        // active account snapshot. Each API key has its own credential blob;
        // the account record only stores the selected bank and item count.
        // A native read failure cannot be treated as an empty store: guessing
        // bank "a" could overwrite the active bank before the account record
        // commit point, making a previously valid snapshot unreadable.
        let old_record = load_account_record_with(&mut load)?;
        let old_bank = previous_bank_from_record(old_record.as_deref())?;
        let bank = match old_bank.as_ref() {
            Some((bank, _)) if bank == "a" => "b",
            _ => "a",
        };
        validate_key_count(keys.len())?;
        let account = v2_account_payload(session.to_persisted_value(), bank, keys.len());
        // Preflight every serialized item before the first native write. A
        // capacity failure therefore leaves the currently active bank and
        // account record untouched.
        let serialized_account = bounded_json(&account)?;
        for key in keys {
            let payload = json!({
                "version": Z8_CREDENTIAL_FORMAT_VERSION,
                "key": key.to_persisted_value(),
            });
            bounded_json(&payload)?;
        }
        let key_writes = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let payload = json!({
                    "version": Z8_CREDENTIAL_FORMAT_VERSION,
                    "key": key.to_persisted_value(),
                });
                Ok((key_target(bank, index), bounded_json(&payload)?))
            })
            .collect::<Result<Vec<_>, _>>()?;
        // A previous logout can leave orphaned Key records while its
        // tombstone correctly suppresses account restoration. Before a new
        // login removes that marker, clear both bounded banks. A failed
        // deletion leaves the tombstone in place and prevents the new account
        // from making the abandoned Key reachable again.
        if load(Z8_LOGOUT_TOMBSTONE_TARGET)?.is_some() {
            clear_bank_with("a", MAX_PERSISTED_KEYS, &mut clear)?;
            clear_bank_with("b", MAX_PERSISTED_KEYS, &mut clear)?;
        }
        save_snapshot_with(
            &key_writes,
            Z8_CREDENTIAL_ACCOUNT,
            &serialized_account,
            old_bank
                .as_ref()
                .map(|(bank, count)| (bank.as_str(), *count)),
            save,
            clear,
        )?;
        Ok(())
    }

    /// Load the account snapshot. Missing credentials are a normal first-run
    /// state and return `Ok(None)`.
    pub fn load(&self) -> Result<Option<PersistedAccount>, SecureStoreError> {
        load_snapshot_with(platform::load)
    }

    /// Delete all saved account material during logout.
    pub fn clear(&self) -> Result<(), SecureStoreError> {
        clear_snapshot_with(platform::save, platform::clear)
    }
}

fn load_snapshot_with<L>(mut load: L) -> Result<Option<PersistedAccount>, SecureStoreError>
where
    L: FnMut(&str) -> Result<Option<String>, SecureStoreError>,
{
    // If logout could not remove one of the native records, suppress
    // restoration until a new login writes a complete account snapshot.
    // This prevents z8_account_status from reviving a session the user
    // explicitly logged out of.
    if load(Z8_LOGOUT_TOMBSTONE_TARGET)?.is_some() {
        return Ok(None);
    }
    let Some(serialized) = load_account_record_with(&mut load)? else {
        return Ok(None);
    };
    let (session_value, bank, count) = parse_v2_account_owned(&serialized)?;
    let mut keys = Vec::with_capacity(count);
    for index in 0..count {
        let value = load(&key_target(&bank, index))?.ok_or(SecureStoreError::InvalidPayload)?;
        let payload: Value =
            serde_json::from_str(&value).map_err(|_| SecureStoreError::InvalidPayload)?;
        keys.push(parse_v2_key_payload(&payload)?);
    }
    let session = AccountSession::from_persisted_value(&session_value)
        .map_err(|_| SecureStoreError::InvalidPayload)?;
    Ok(Some(PersistedAccount { session, keys }))
}

/// Persist a complete account snapshot while retaining a logout tombstone
/// until the account record is durable. This ordering is the important
/// failure boundary: a failed login write leaves an existing account visible,
/// while a failed tombstone removal leaves the new account hidden (fail closed)
/// instead of exposing an older logged-out account.
fn save_snapshot_with<S, C>(
    key_writes: &[(String, String)],
    account_target: &str,
    account_payload: &str,
    old_bank: Option<(&str, usize)>,
    mut save: S,
    mut clear: C,
) -> Result<(), SecureStoreError>
where
    S: FnMut(&str, &str) -> Result<(), SecureStoreError>,
    C: FnMut(&str) -> Result<(), SecureStoreError>,
{
    for (target, payload) in key_writes {
        save(target, payload)?;
    }
    // The account record is the commit point for the alternate bank.
    save(account_target, account_payload)?;
    // Old-bank cleanup is deliberately best effort. The account record above
    // already points exclusively at the new bank.
    if let Some((old_bank, old_count)) = old_bank {
        let _ = clear_bank_with(old_bank, MAX_PERSISTED_KEYS.max(old_count), &mut clear);
    }
    // Only now is it safe to unmask the new snapshot. If this fails, the
    // tombstone remains and a restart cannot resurrect the old account.
    clear(Z8_LOGOUT_TOMBSTONE_TARGET)
}

/// Establish a durable logout marker before deleting any account record. If
/// marker creation fails, no deletion is attempted: the caller reports a
/// failed logout rather than claiming a state that could resurrect on restart.
fn clear_snapshot_with<S, C>(mut save: S, mut clear: C) -> Result<(), SecureStoreError>
where
    S: FnMut(&str, &str) -> Result<(), SecureStoreError>,
    C: FnMut(&str) -> Result<(), SecureStoreError>,
{
    save(Z8_LOGOUT_TOMBSTONE_TARGET, Z8_LOGOUT_TOMBSTONE_PAYLOAD)?;
    let mut first_error = None;
    // Both banks are bounded and are cleared even if the account record is
    // malformed or a prior write stopped halfway through.
    for bank in ["a", "b"] {
        if let Err(error) = clear_bank_with(bank, MAX_PERSISTED_KEYS, &mut clear) {
            first_error = first_error.or(Some(error));
        }
    }
    if let Err(error) = clear(Z8_CREDENTIAL_ACCOUNT) {
        first_error = first_error.or(Some(error));
    }
    if let Some(error) = first_error {
        // The tombstone remains in place, so a later status call stays
        // fail-closed even though one or more native deletes failed.
        return Err(error);
    }
    // A successful cleanup can finally remove the marker. If this final
    // deletion fails, the marker still protects against account resurrection.
    clear(Z8_LOGOUT_TOMBSTONE_TARGET)
}

/*
 * The methods below intentionally remain private to this module. They are
 * kept together with the injected sequencing helpers above so platform mock
 * tests can exercise failure boundaries without touching a user's vault.
 */

/*
 * (The implementation of `Z8SecureStore::load` and the parsers continues
 * below; this comment marks the end of the lifecycle sequencing section.)
 */

fn parse_api_key_value(value: &Value) -> Result<AccountApiKey, SecureStoreError> {
    AccountApiKey::from_persisted_value(value).map_err(|_| SecureStoreError::InvalidPayload)
}

fn parse_v2_key_payload(payload: &Value) -> Result<AccountApiKey, SecureStoreError> {
    if payload.get("version").and_then(Value::as_u64) != Some(Z8_CREDENTIAL_FORMAT_VERSION) {
        return Err(SecureStoreError::InvalidPayload);
    }
    payload
        .get("key")
        .ok_or(SecureStoreError::InvalidPayload)
        .and_then(parse_api_key_value)
}

fn key_target(bank: &str, index: usize) -> String {
    format!("{Z8_CREDENTIAL_NAMESPACE}:api-key:{bank}:{index}")
}

fn v2_account_payload(session: Value, bank: &str, count: usize) -> Value {
    json!({
        "version": Z8_CREDENTIAL_FORMAT_VERSION,
        "session": session,
        "keys": { "bank": bank, "count": count },
    })
}

fn validate_key_count(count: usize) -> Result<(), SecureStoreError> {
    if count > MAX_PERSISTED_KEYS {
        return Err(SecureStoreError::Backend {
            operation: "save (too many credentials)",
        });
    }
    Ok(())
}

fn load_account_record_with<L>(mut load: L) -> Result<Option<String>, SecureStoreError>
where
    L: FnMut(&str) -> Result<Option<String>, SecureStoreError>,
{
    load(Z8_CREDENTIAL_ACCOUNT)
}

fn previous_bank_from_record(
    value: Option<&str>,
) -> Result<Option<(String, usize)>, SecureStoreError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let (_, bank, count) = parse_v2_account_owned(value)?;
    Ok(Some((bank, count)))
}

fn bounded_json(payload: &Value) -> Result<String, SecureStoreError> {
    let serialized = serde_json::to_string(payload).map_err(|_| SecureStoreError::Serialization)?;
    if serialized.len() > MAX_CREDENTIAL_BLOB_BYTES {
        return Err(SecureStoreError::Backend {
            operation: "save (credential too large)",
        });
    }
    Ok(serialized)
}

fn parse_v2_account_owned(value: &str) -> Result<(Value, String, usize), SecureStoreError> {
    let payload: Value =
        serde_json::from_str(value).map_err(|_| SecureStoreError::InvalidPayload)?;
    if payload.get("version").and_then(Value::as_u64) != Some(Z8_CREDENTIAL_FORMAT_VERSION) {
        return Err(SecureStoreError::InvalidPayload);
    }
    let keys = payload
        .get("keys")
        .and_then(Value::as_object)
        .ok_or(SecureStoreError::InvalidPayload)?;
    let bank = keys
        .get("bank")
        .and_then(Value::as_str)
        .filter(|bank| matches!(*bank, "a" | "b"))
        .ok_or(SecureStoreError::InvalidPayload)?
        .to_string();
    let count = keys
        .get("count")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or(SecureStoreError::InvalidPayload)?;
    validate_key_count(count).map_err(|_| SecureStoreError::InvalidPayload)?;
    Ok((
        payload
            .get("session")
            .cloned()
            .ok_or(SecureStoreError::InvalidPayload)?,
        bank,
        count,
    ))
}

fn clear_bank_with<F>(bank: &str, count: usize, mut clear: F) -> Result<(), SecureStoreError>
where
    F: FnMut(&str) -> Result<(), SecureStoreError>,
{
    let mut first_error = None;
    for index in 0..count {
        if let Err(error) = clear(&key_target(bank, index)) {
            first_error = first_error.or(Some(error));
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn decode_credential_blob(bytes: &[u8]) -> Result<String, SecureStoreError> {
    if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_BLOB_BYTES {
        return Err(SecureStoreError::InvalidPayload);
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| SecureStoreError::InvalidPayload)
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;
    use std::slice;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };

    fn wide(value: &str) -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(Some(0))
            .collect()
    }

    pub(super) unsafe fn credential_blob_to_string(
        credential: *const CREDENTIALW,
    ) -> Result<String, SecureStoreError> {
        let credential = unsafe {
            credential
                .as_ref()
                .ok_or(SecureStoreError::InvalidPayload)?
        };
        let length = credential.CredentialBlobSize as usize;
        if length == 0 || length > MAX_CREDENTIAL_BLOB_BYTES {
            return Err(SecureStoreError::InvalidPayload);
        }
        if credential.CredentialBlob.is_null() {
            return Err(SecureStoreError::InvalidPayload);
        }
        let bytes = unsafe { slice::from_raw_parts(credential.CredentialBlob, length) };
        decode_credential_blob(bytes)
    }

    pub fn save(target_name: &str, value: &str) -> Result<(), SecureStoreError> {
        let mut target = wide(target_name);
        let mut username = wide(Z8_CREDENTIAL_ACCOUNT);
        let mut blob = value.as_bytes().to_vec();
        if blob.is_empty() || blob.len() > MAX_CREDENTIAL_BLOB_BYTES {
            return Err(SecureStoreError::Backend {
                operation: "save (credential too large)",
            });
        }
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            CredentialBlobSize: blob
                .len()
                .try_into()
                .map_err(|_| SecureStoreError::Backend { operation: "save" })?,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: PWSTR(username.as_mut_ptr()),
            ..Default::default()
        };
        unsafe { CredWriteW(&credential, 0) }
            .map_err(|_| SecureStoreError::Backend { operation: "save" })
    }

    pub fn load(target_name: &str) -> Result<Option<String>, SecureStoreError> {
        let target = wide(target_name);
        let mut credential: *mut CREDENTIALW = null_mut();
        let result = unsafe {
            CredReadW(
                PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                0,
                &mut credential,
            )
        };
        if let Err(error) = result {
            if error.code() == ERROR_NOT_FOUND.to_hresult() {
                return Ok(None);
            }
            return Err(SecureStoreError::Backend { operation: "load" });
        }
        let value = unsafe { credential_blob_to_string(credential) };
        if !credential.is_null() {
            unsafe { CredFree(credential.cast()) };
        }
        value.map(Some)
    }

    pub fn clear(target_name: &str) -> Result<(), SecureStoreError> {
        let target = wide(target_name);
        let result = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, 0) };
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
            Err(_) => Err(SecureStoreError::Backend { operation: "clear" }),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    pub fn save(target: &str, value: &str) -> Result<(), SecureStoreError> {
        set_generic_password(Z8_CREDENTIAL_SERVICE, target, value.as_bytes())
            .map_err(|_| SecureStoreError::Backend { operation: "save" })
    }

    pub fn load(target: &str) -> Result<Option<String>, SecureStoreError> {
        match get_generic_password(Z8_CREDENTIAL_SERVICE, target) {
            Ok(value) => decode_credential_blob(&value).map(Some),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(_) => Err(SecureStoreError::Backend { operation: "load" }),
        }
    }

    pub fn clear(target: &str) -> Result<(), SecureStoreError> {
        match delete_generic_password(Z8_CREDENTIAL_SERVICE, target) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(_) => Err(SecureStoreError::Backend { operation: "clear" }),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::*;

    pub fn save(_: &str, _: &str) -> Result<(), SecureStoreError> {
        Err(SecureStoreError::Backend {
            operation: "save (unsupported platform)",
        })
    }

    pub fn load(_: &str) -> Result<Option<String>, SecureStoreError> {
        Err(SecureStoreError::Backend {
            operation: "load (unsupported platform)",
        })
    }

    pub fn clear(_: &str) -> Result<(), SecureStoreError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    #[cfg(windows)]
    const NATIVE_TEST_TARGET_PREFIX: &str = "z8-codex-native-vault-test:";

    #[cfg(windows)]
    #[test]
    fn windows_native_credential_child() {
        // Ordinary unit-test runs never access the native vault. The parent
        // test launches this exact test name in a separate process.
        if std::env::var("Z8_NATIVE_VAULT_TEST_CHILD").as_deref() != Ok("1") {
            return;
        }
        let target = std::env::var("Z8_NATIVE_VAULT_TEST_TARGET").unwrap();
        let id = target
            .strip_prefix(NATIVE_TEST_TARGET_PREFIX)
            .and_then(|value| uuid::Uuid::parse_str(value).ok().map(|id| (value, id)))
            .filter(|(value, id)| id.hyphenated().to_string() == *value)
            .expect("native probe must use a unique test-only target");
        let value = format!("synthetic-cross-process:{}", id.1);
        match std::env::var("Z8_NATIVE_VAULT_TEST_ACTION")
            .unwrap()
            .as_str()
        {
            "absent" => assert_eq!(platform::load(&target).unwrap(), None),
            "save" => {
                assert_eq!(platform::load(&target).unwrap(), None);
                platform::save(&target, &value).unwrap();
            }
            "load" => assert_eq!(
                platform::load(&target).unwrap().as_deref(),
                Some(value.as_str())
            ),
            "clear" => platform::clear(&target).unwrap(),
            _ => panic!("unknown native probe action"),
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "writes only a random test target in Windows Credential Manager"]
    fn windows_native_credential_round_trip_across_processes() {
        use std::process::Command;

        let target = format!("{NATIVE_TEST_TARGET_PREFIX}{}", uuid::Uuid::new_v4());
        let executable = std::env::current_exe().unwrap();
        let run_child = |action: &str| {
            let output = Command::new(&executable)
                .arg("--exact")
                .arg("z8_secure_store::tests::windows_native_credential_child")
                .arg("--nocapture")
                .env("Z8_NATIVE_VAULT_TEST_CHILD", "1")
                .env("Z8_NATIVE_VAULT_TEST_TARGET", &target)
                .env("Z8_NATIVE_VAULT_TEST_ACTION", action)
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
                "native probe {action} failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };

        run_child("absent");
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Err(error) = platform::clear(&self.0) {
                    eprintln!("native probe cleanup failed: {error}");
                }
            }
        }
        let _cleanup = Cleanup(target.clone());
        run_child("save");
        run_child("load");
        run_child("clear");
        run_child("absent");
    }

    #[derive(Default)]
    struct MemoryVault {
        records: RefCell<HashMap<String, String>>,
        fail_save: RefCell<Option<String>>,
        fail_clear: RefCell<Option<String>>,
    }

    impl MemoryVault {
        fn load(&self, target: &str) -> Result<Option<String>, SecureStoreError> {
            Ok(self.records.borrow().get(target).cloned())
        }

        fn save(&self, target: &str, payload: &str) -> Result<(), SecureStoreError> {
            if self.fail_save.borrow().as_deref() == Some(target) {
                return Err(SecureStoreError::Backend { operation: "save" });
            }
            self.records
                .borrow_mut()
                .insert(target.to_string(), payload.to_string());
            Ok(())
        }

        fn clear(&self, target: &str) -> Result<(), SecureStoreError> {
            if self.fail_clear.borrow().as_deref() == Some(target) {
                return Err(SecureStoreError::Backend { operation: "clear" });
            }
            self.records.borrow_mut().remove(target);
            Ok(())
        }

        fn save_snapshot(
            &self,
            session: &AccountSession,
            keys: &[AccountApiKey],
        ) -> Result<(), SecureStoreError> {
            Z8SecureStore::new().save_with(
                session,
                keys,
                |target| self.load(target),
                |target, value| self.save(target, value),
                |target| self.clear(target),
            )
        }

        fn recover(&self) -> Result<Option<PersistedAccount>, SecureStoreError> {
            load_snapshot_with(|target| self.load(target))
        }

        fn logout(&self) -> Result<(), SecureStoreError> {
            clear_snapshot_with(
                |target, value| self.save(target, value),
                |target| self.clear(target),
            )
        }
    }

    fn fixture_session(token: &str) -> AccountSession {
        AccountSession::from_persisted_value(&json!({
            "access_token": token,
            "refresh_token": format!("refresh-{token}"),
            "user": {"email": "user@example.com"}
        }))
        .unwrap()
    }

    fn fixture_key(id: &str) -> AccountApiKey {
        AccountApiKey::from_persisted_value(&json!({
            "id": "1",
            "name": "fixture",
            "status": "active",
            "key": format!("synthetic-{id}")
        }))
        .unwrap()
    }

    #[test]
    fn restart_after_second_inactive_bank_write_failure_restores_old_snapshot() {
        let vault = MemoryVault::default();
        vault
            .save_snapshot(&fixture_session("old"), &[fixture_key("old")])
            .unwrap();
        *vault.fail_save.borrow_mut() = Some(key_target("b", 1));

        assert_eq!(
            vault.save_snapshot(
                &fixture_session("new"),
                &[fixture_key("new-1"), fixture_key("new-2")]
            ),
            Err(SecureStoreError::Backend { operation: "save" })
        );
        let restored = vault.recover().unwrap().unwrap();
        assert_eq!(restored.session.access_token(), "old");
        assert_eq!(restored.keys.len(), 1);
        assert_eq!(restored.keys[0].secret(), "synthetic-old");

        *vault.fail_save.borrow_mut() = None;
        vault
            .save_snapshot(&fixture_session("new"), &[fixture_key("new-1")])
            .unwrap();
        let restored = vault.recover().unwrap().unwrap();
        assert_eq!(restored.session.access_token(), "new");
        assert_eq!(restored.keys[0].secret(), "synthetic-new-1");
    }

    #[test]
    fn failed_logout_cleanup_stays_hidden_across_restart() {
        let vault = MemoryVault::default();
        vault
            .save_snapshot(&fixture_session("old"), &[fixture_key("old")])
            .unwrap();
        *vault.fail_clear.borrow_mut() = Some(Z8_CREDENTIAL_ACCOUNT.to_string());

        assert_eq!(
            vault.logout(),
            Err(SecureStoreError::Backend { operation: "clear" })
        );
        assert!(vault.recover().unwrap().is_none());
        assert!(vault
            .records
            .borrow()
            .contains_key(Z8_LOGOUT_TOMBSTONE_TARGET));
    }

    #[test]
    fn failed_old_bank_cleanup_does_not_hide_committed_new_snapshot() {
        let vault = MemoryVault::default();
        vault
            .save_snapshot(&fixture_session("old"), &[fixture_key("old")])
            .unwrap();
        let old_key = key_target("a", 0);
        *vault.fail_clear.borrow_mut() = Some(old_key.clone());

        vault
            .save_snapshot(&fixture_session("new"), &[fixture_key("new")])
            .unwrap();
        let restored = vault.recover().unwrap().unwrap();
        assert_eq!(restored.session.access_token(), "new");
        assert_eq!(restored.keys[0].secret(), "synthetic-new");
        assert!(vault.records.borrow().contains_key(&old_key));

        *vault.fail_clear.borrow_mut() = None;
        vault.logout().unwrap();
        assert!(vault.recover().unwrap().is_none());
        assert!(!vault.records.borrow().contains_key(&old_key));
    }

    #[test]
    fn failed_logout_marker_write_preserves_previous_restart_snapshot() {
        let vault = MemoryVault::default();
        vault
            .save_snapshot(&fixture_session("old"), &[fixture_key("old")])
            .unwrap();
        *vault.fail_save.borrow_mut() = Some(Z8_LOGOUT_TOMBSTONE_TARGET.to_string());

        assert_eq!(
            vault.logout(),
            Err(SecureStoreError::Backend { operation: "save" })
        );
        assert_eq!(
            vault.recover().unwrap().unwrap().session.access_token(),
            "old"
        );
    }

    #[test]
    fn new_login_must_not_unmask_orphaned_key_from_failed_logout() {
        let vault = MemoryVault::default();
        vault
            .save_snapshot(&fixture_session("old"), &[fixture_key("old")])
            .unwrap();
        let orphan_target = key_target("a", 0);
        *vault.fail_clear.borrow_mut() = Some(orphan_target.clone());
        assert_eq!(
            vault.logout(),
            Err(SecureStoreError::Backend { operation: "clear" })
        );
        assert!(vault.recover().unwrap().is_none());
        assert!(vault.records.borrow().contains_key(&orphan_target));

        // The account record was deleted. A following login with no keys must
        // not remove the marker while the old key still cannot be deleted.
        assert_eq!(
            vault.save_snapshot(&fixture_session("new"), &[]),
            Err(SecureStoreError::Backend { operation: "clear" }),
            "unresolved logout cleanup must keep the tombstone"
        );
        assert!(vault.recover().unwrap().is_none());
        assert!(vault
            .records
            .borrow()
            .contains_key(Z8_LOGOUT_TOMBSTONE_TARGET));
        assert!(vault.records.borrow().contains_key(&orphan_target));

        *vault.fail_clear.borrow_mut() = None;
        vault.save_snapshot(&fixture_session("new"), &[]).unwrap();
        let restored = vault.recover().unwrap().unwrap();
        assert_eq!(restored.session.access_token(), "new");
        assert!(restored.keys.is_empty());
        assert!(!vault.records.borrow().contains_key(&orphan_target));
        assert!(!vault
            .records
            .borrow()
            .contains_key(Z8_LOGOUT_TOMBSTONE_TARGET));
    }

    #[test]
    fn credential_identity_is_stable_and_brand_scoped() {
        assert_eq!(Z8_CREDENTIAL_SERVICE, "com.z8.codex.manager");
        assert_eq!(Z8_CREDENTIAL_NAMESPACE, "com.z8.codex.manager:v3");
        assert_eq!(Z8_CREDENTIAL_ACCOUNT, "com.z8.codex.manager:v3:account");
        assert_eq!(
            Z8_LOGOUT_TOMBSTONE_TARGET,
            "com.z8.codex.manager:v3:logout-pending"
        );
    }

    #[test]
    fn login_restore_and_logout_never_touch_development_targets() {
        let vault = MemoryVault::default();
        // These include the old bare Windows generic targets, the old Z8
        // single-blob target and both old API Key banks. They may belong to
        // another application, so even cleanup must leave them untouched.
        let legacy = HashMap::from([
            (
                "account".to_string(),
                v2_account_payload(fixture_session("old").to_persisted_value(), "a", 1).to_string(),
            ),
            (
                "logout-pending".to_string(),
                Z8_LOGOUT_TOMBSTONE_PAYLOAD.to_string(),
            ),
            (
                "com.z8.codex.manager".to_string(),
                "old-single-blob".to_string(),
            ),
            (
                "com.z8.codex.manager:api-key:a:0".to_string(),
                "old-a-key".to_string(),
            ),
            (
                "com.z8.codex.manager:api-key:b:0".to_string(),
                "old-b-key".to_string(),
            ),
        ]);
        vault.records.borrow_mut().extend(legacy.clone());
        let only_new_target = |target: &str| {
            assert!(
                target.starts_with("com.z8.codex.manager:v3:"),
                "native operation crossed the Z8 v3 boundary: {target}"
            );
        };
        // An upgrade sees only old targets, so it must prompt for a new login
        // even if the old entry contains a valid Z8 session.
        assert!(load_snapshot_with(|target| {
            only_new_target(target);
            vault.load(target)
        })
        .unwrap()
        .is_none());

        let session = fixture_session("new");
        let key = fixture_key("new");
        Z8SecureStore::new()
            .save_with(
                &session,
                &[key],
                |target| {
                    only_new_target(target);
                    vault.load(target)
                },
                |target, payload| {
                    only_new_target(target);
                    vault.save(target, payload)
                },
                |target| {
                    only_new_target(target);
                    vault.clear(target)
                },
            )
            .unwrap();
        let restored = load_snapshot_with(|target| {
            only_new_target(target);
            vault.load(target)
        })
        .unwrap()
        .unwrap();
        assert_eq!(restored.session.access_token(), "new");
        assert_eq!(restored.keys[0].secret(), "synthetic-new");

        clear_snapshot_with(
            |target, payload| {
                only_new_target(target);
                vault.save(target, payload)
            },
            |target| {
                only_new_target(target);
                vault.clear(target)
            },
        )
        .unwrap();
        assert!(load_snapshot_with(|target| {
            only_new_target(target);
            vault.load(target)
        })
        .unwrap()
        .is_none());
        *vault.fail_save.borrow_mut() = Some(Z8_CREDENTIAL_ACCOUNT.to_string());
        assert_eq!(
            vault.save_snapshot(&fixture_session("failed"), &[]),
            Err(SecureStoreError::Backend { operation: "save" })
        );
        *vault.fail_save.borrow_mut() = None;
        assert!(vault.recover().unwrap().is_none());
        for (target, payload) in legacy {
            assert_eq!(vault.records.borrow().get(&target), Some(&payload));
        }
    }

    #[test]
    fn logout_tombstone_is_small_and_json_encoded() {
        let payload: Value = serde_json::from_str(Z8_LOGOUT_TOMBSTONE_PAYLOAD).unwrap();
        assert_eq!(payload["version"], 1);
        assert!(Z8_LOGOUT_TOMBSTONE_PAYLOAD.len() < MAX_CREDENTIAL_BLOB_BYTES);
    }

    #[test]
    fn persisted_payload_round_trips_without_plaintext_file_api() {
        let session = AccountSession::from_persisted_value(&json!({
            "access_token": "access-secret",
            "refresh_token": "refresh-secret",
            "user": {"email": "user@example.com"},
            "expires_at": "2099-01-01T00:00:00Z"
        }))
        .unwrap();
        let key = AccountApiKey::from_persisted_value(&json!({
            "id": "1",
            "name": "primary",
            "status": "active",
            "key": "api-secret"
        }))
        .unwrap();
        let payload = v2_account_payload(session.to_persisted_value(), "a", 1);
        let key_payload = json!({
            "version": Z8_CREDENTIAL_FORMAT_VERSION,
            "key": key.to_persisted_value()
        });
        assert_eq!(payload["version"], Z8_CREDENTIAL_FORMAT_VERSION);
        assert_eq!(payload["session"]["refresh_token"], "refresh-secret");
        assert_eq!(key_payload["key"]["key"], "api-secret");
        assert!(!format!("{session:?}").contains("access-secret"));
        assert!(!format!("{session:?}").contains("refresh-secret"));
        assert!(!format!("{key:?}").contains("api-secret"));
    }

    #[test]
    fn malformed_or_wrong_version_payload_is_rejected() {
        let vault = MemoryVault::default();
        vault.records.borrow_mut().insert(
            Z8_CREDENTIAL_ACCOUNT.to_string(),
            json!({"version": 1, "session": fixture_session("legacy").to_persisted_value(), "keys": []}).to_string(),
        );
        assert!(matches!(
            vault.recover(),
            Err(SecureStoreError::InvalidPayload)
        ));
    }

    #[test]
    fn v2_account_record_keeps_bank_metadata_small() {
        let session = json!({
            "access_token": "access-secret",
            "refresh_token": "refresh-secret",
            "user": {"email": "user@example.com"},
            "expires_at": "2099-01-01T00:00:00Z"
        });
        let payload = json!({
            "version": Z8_CREDENTIAL_FORMAT_VERSION,
            "session": session,
            "keys": {"bank": "b", "count": MAX_PERSISTED_KEYS}
        });
        let serialized = serde_json::to_string(&payload).unwrap();
        let (_, bank, count) = parse_v2_account_owned(&serialized).unwrap();
        assert_eq!(bank, "b");
        assert_eq!(count, MAX_PERSISTED_KEYS);
        assert!(serialized.len() < MAX_CREDENTIAL_BLOB_BYTES);
    }

    #[test]
    fn v2_account_record_rejects_unbounded_key_count() {
        let payload = v2_account_payload(
            json!({
                "access_token": "access-secret",
                "refresh_token": "refresh-secret",
                "user": {"email": "user@example.com"},
            }),
            "a",
            MAX_PERSISTED_KEYS + 1,
        );
        let serialized = serde_json::to_string(&payload).unwrap();
        assert_eq!(
            parse_v2_account_owned(&serialized),
            Err(SecureStoreError::InvalidPayload)
        );
    }

    #[test]
    fn saving_aborts_before_any_write_when_native_account_read_fails() {
        let session = AccountSession::from_persisted_value(&json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "user": {"email": "user@example.com"}
        }))
        .unwrap();
        let writes = Rc::new(RefCell::new(Vec::<String>::new()));
        let key = AccountApiKey::from_persisted_value(&json!({
            "id": "1", "name": "fixture", "status": "active", "key": "synthetic-key"
        }))
        .unwrap();
        let save_writes = Rc::clone(&writes);
        let clear_writes = Rc::clone(&writes);
        let result = Z8SecureStore::new().save_with(
            &session,
            &[key],
            |target| {
                assert_eq!(target, Z8_CREDENTIAL_ACCOUNT);
                Err(SecureStoreError::Backend { operation: "load" })
            },
            move |target, _| {
                save_writes.borrow_mut().push(format!("save:{target}"));
                Ok(())
            },
            move |target| {
                clear_writes.borrow_mut().push(format!("clear:{target}"));
                Ok(())
            },
        );
        assert_eq!(result, Err(SecureStoreError::Backend { operation: "load" }));
        assert!(writes.borrow().is_empty());
    }

    #[test]
    fn new_login_never_reads_an_unowned_legacy_target() {
        let session = AccountSession::from_persisted_value(&json!({
            "access_token": "new-access",
            "user": {"email": "user@example.com"}
        }))
        .unwrap();
        let mut reads = Vec::new();
        let mut writes = 0;
        let result = Z8SecureStore::new().save_with(
            &session,
            &[],
            |target| {
                reads.push(target.to_string());
                assert!(matches!(
                    target,
                    Z8_CREDENTIAL_ACCOUNT | Z8_LOGOUT_TOMBSTONE_TARGET
                ));
                Ok(None)
            },
            |_, _| {
                writes += 1;
                Ok(())
            },
            |_| Ok(()),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(reads, [Z8_CREDENTIAL_ACCOUNT, Z8_LOGOUT_TOMBSTONE_TARGET]);
        assert_eq!(writes, 1);
    }

    #[test]
    fn failed_new_bank_write_does_not_touch_active_bank() {
        let session = AccountSession::from_persisted_value(&json!({
            "access_token": "new-access",
            "user": {"email": "user@example.com"}
        }))
        .unwrap();
        let account = v2_account_payload(session.to_persisted_value(), "a", 1).to_string();
        let key = AccountApiKey::from_persisted_value(&json!({
            "id": "1", "name": "fixture", "status": "active", "key": "synthetic-key"
        }))
        .unwrap();
        let mut writes = Vec::new();
        let result = Z8SecureStore::new().save_with(
            &session,
            &[key],
            |target| {
                if target == Z8_LOGOUT_TOMBSTONE_TARGET {
                    return Ok(None);
                }
                assert_eq!(target, Z8_CREDENTIAL_ACCOUNT);
                Ok(Some(account.clone()))
            },
            |target, _| {
                writes.push(target.to_string());
                Err(SecureStoreError::Backend { operation: "save" })
            },
            |_| panic!("failed write must not start cleanup"),
        );
        assert_eq!(result, Err(SecureStoreError::Backend { operation: "save" }));
        assert_eq!(writes, [key_target("b", 0)]);
    }

    #[test]
    fn saving_rejects_unknown_active_bank_instead_of_guessing_a() {
        let session = AccountSession::from_persisted_value(&json!({
            "access_token": "new-access",
            "user": {"email": "user@example.com"}
        }))
        .unwrap();
        let account = json!({
            "version": Z8_CREDENTIAL_FORMAT_VERSION,
            "session": session.to_persisted_value(),
            "keys": {"bank": "unknown", "count": 1}
        })
        .to_string();
        let mut writes = 0;
        let result = Z8SecureStore::new().save_with(
            &session,
            &[],
            |target| {
                assert_eq!(target, Z8_CREDENTIAL_ACCOUNT);
                Ok(Some(account.clone()))
            },
            |_, _| {
                writes += 1;
                Ok(())
            },
            |_| Ok(()),
        );
        assert_eq!(result, Err(SecureStoreError::InvalidPayload));
        assert_eq!(writes, 0);
    }

    #[test]
    fn old_format_in_new_namespace_is_rejected() {
        let legacy = json!({
            "version": 1,
            "session": {
                "access_token": "legacy-access",
                "refresh_token": "legacy-refresh",
                "user": {"email": "user@example.com"}
            },
            "keys": []
        })
        .to_string();
        assert_eq!(
            previous_bank_from_record(Some(&legacy)),
            Err(SecureStoreError::InvalidPayload)
        );
        assert_eq!(previous_bank_from_record(None), Ok(None));
    }

    #[test]
    fn v2_key_record_uses_the_same_parser_as_production_load() {
        let payload = json!({
            "version": Z8_CREDENTIAL_FORMAT_VERSION,
            "key": {
                "id": "1",
                "name": "primary",
                "status": "active",
                "key": "api-secret"
            }
        });
        let parsed = parse_v2_key_payload(&payload).unwrap();
        assert_eq!(parsed.id, "1");
        assert_eq!(parsed.secret(), "api-secret");
    }

    #[test]
    fn v2_key_record_rejects_wrong_version_and_missing_key() {
        assert_eq!(
            parse_v2_key_payload(&json!({
                "version": 1,
                "key": {"id": "1", "key": "api-secret"}
            })),
            Err(SecureStoreError::InvalidPayload)
        );
        assert_eq!(
            parse_v2_key_payload(&json!({"version": Z8_CREDENTIAL_FORMAT_VERSION})),
            Err(SecureStoreError::InvalidPayload)
        );
    }

    #[test]
    fn exact_blob_limit_is_accepted_and_next_byte_is_rejected() {
        let overhead = serde_json::to_string(&json!({ "value": "" }))
            .unwrap()
            .len();
        let exact = json!({
            "value": "x".repeat(MAX_CREDENTIAL_BLOB_BYTES - overhead)
        });
        assert_eq!(
            bounded_json(&exact).unwrap().len(),
            MAX_CREDENTIAL_BLOB_BYTES
        );

        let oversized = json!({
            "value": "x".repeat(MAX_CREDENTIAL_BLOB_BYTES - overhead + 1)
        });
        assert_eq!(
            bounded_json(&oversized),
            Err(SecureStoreError::Backend {
                operation: "save (credential too large)"
            })
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_blob_limit_matches_credentialw_bytes_not_kibibytes() {
        // Microsoft documents CRED_MAX_CREDENTIAL_BLOB_SIZE as 5 * 512.
        // This independent number catches a future accidental return to 5 KiB.
        assert_eq!(MAX_CREDENTIAL_BLOB_BYTES, 2560);
        assert_eq!(
            decode_credential_blob(&vec![b'x'; 2560]).unwrap().len(),
            2560
        );
        assert_eq!(
            decode_credential_blob(&vec![b'x'; 2561]),
            Err(SecureStoreError::InvalidPayload)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keychain_keeps_previous_five_kibibyte_bound() {
        assert_eq!(MAX_CREDENTIAL_BLOB_BYTES, 5120);
    }

    #[cfg(windows)]
    #[test]
    fn oversized_last_key_aborts_before_any_native_write() {
        let session = AccountSession::from_persisted_value(&json!({
            "access_token": "new-access",
            "user": {"email": "user@example.com"}
        }))
        .unwrap();
        let old_record = v2_account_payload(session.to_persisted_value(), "a", 1).to_string();
        let small_key = AccountApiKey::from_persisted_value(&json!({
            "id": "1", "name": "small", "status": "active", "key": "synthetic-key"
        }))
        .unwrap();
        // 3000 bytes passed the old 5 KiB preflight but exceeds CREDENTIALW's
        // real 2560-byte limit. Put it after a valid key to catch partial bank writes.
        let large_key = AccountApiKey::from_persisted_value(&json!({
            "id": "2", "name": "large", "status": "active", "key": "x".repeat(3000)
        }))
        .unwrap();
        let mut writes = 0;
        let result = Z8SecureStore::new().save_with(
            &session,
            &[small_key, large_key],
            |target| {
                assert_eq!(target, Z8_CREDENTIAL_ACCOUNT);
                Ok(Some(old_record.clone()))
            },
            |_, _| {
                writes += 1;
                Ok(())
            },
            |_| panic!("capacity failure must not begin cleanup"),
        );
        assert_eq!(
            result,
            Err(SecureStoreError::Backend {
                operation: "save (credential too large)"
            })
        );
        assert_eq!(
            writes, 0,
            "all records must pass preflight before the first write"
        );
    }

    #[test]
    fn credential_blob_decoder_rejects_empty_oversized_and_invalid_utf8() {
        assert_eq!(
            decode_credential_blob(&[]),
            Err(SecureStoreError::InvalidPayload)
        );
        assert_eq!(
            decode_credential_blob(&vec![b'x'; MAX_CREDENTIAL_BLOB_BYTES + 1]),
            Err(SecureStoreError::InvalidPayload)
        );
        assert_eq!(
            decode_credential_blob(&[0xff]),
            Err(SecureStoreError::InvalidPayload)
        );
        assert_eq!(decode_credential_blob(b"{}"), Ok("{}".to_string()));
    }

    #[test]
    fn clear_contract_covers_both_bounded_banks() {
        assert_eq!(key_target("a", 0), "com.z8.codex.manager:v3:api-key:a:0");
        assert_eq!(
            key_target("b", MAX_PERSISTED_KEYS - 1),
            "com.z8.codex.manager:v3:api-key:b:999"
        );
    }

    #[test]
    fn clear_bank_attempts_all_slots_and_returns_the_first_error() {
        let mut targets = Vec::new();
        let result = clear_bank_with("b", 4, |target| {
            targets.push(target.to_string());
            if target.ends_with(":1") {
                Err(SecureStoreError::Backend { operation: "clear" })
            } else {
                Ok(())
            }
        });

        assert_eq!(
            result,
            Err(SecureStoreError::Backend { operation: "clear" })
        );
        assert_eq!(
            targets,
            vec![
                "com.z8.codex.manager:v3:api-key:b:0",
                "com.z8.codex.manager:v3:api-key:b:1",
                "com.z8.codex.manager:v3:api-key:b:2",
                "com.z8.codex.manager:v3:api-key:b:3",
            ]
        );
    }

    #[test]
    fn clear_bank_with_zero_slots_does_not_call_backend() {
        let mut calls = 0;
        assert_eq!(
            clear_bank_with("a", 0, |_| {
                calls += 1;
                Ok(())
            }),
            Ok(())
        );
        assert_eq!(calls, 0);
    }

    #[test]
    fn logout_writes_tombstone_before_any_delete() {
        let events = Rc::new(RefCell::new(Vec::<String>::new()));
        let save_events = Rc::clone(&events);
        let clear_events = Rc::clone(&events);
        let result = clear_snapshot_with(
            move |target, _| {
                save_events.borrow_mut().push(format!("save:{target}"));
                Ok(())
            },
            move |target| {
                if matches!(target, Z8_CREDENTIAL_ACCOUNT | Z8_LOGOUT_TOMBSTONE_TARGET) {
                    clear_events.borrow_mut().push(format!("clear:{target}"));
                }
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
        let events = events.borrow();
        assert_eq!(
            events.first().map(String::as_str),
            Some(format!("save:{Z8_LOGOUT_TOMBSTONE_TARGET}").as_str())
        );
        assert_eq!(
            events.last().map(String::as_str),
            Some(format!("clear:{Z8_LOGOUT_TOMBSTONE_TARGET}").as_str())
        );
        assert!(events
            .iter()
            .any(|event| event == &format!("clear:{Z8_CREDENTIAL_ACCOUNT}")));
    }

    #[test]
    fn logout_marker_write_failure_skips_all_deletes() {
        let events = Rc::new(RefCell::new(Vec::<String>::new()));
        let save_events = Rc::clone(&events);
        let clear_events = Rc::clone(&events);
        let result = clear_snapshot_with(
            move |target, _| {
                save_events.borrow_mut().push(format!("save:{target}"));
                Err(SecureStoreError::Backend { operation: "save" })
            },
            move |target| {
                clear_events.borrow_mut().push(format!("clear:{target}"));
                Ok(())
            },
        );
        assert_eq!(result, Err(SecureStoreError::Backend { operation: "save" }));
        let events = events.borrow();
        assert_eq!(
            events.as_slice(),
            [format!("save:{Z8_LOGOUT_TOMBSTONE_TARGET}")]
        );
    }

    #[test]
    fn logout_delete_failure_keeps_marker_in_place() {
        let events = Rc::new(RefCell::new(Vec::<String>::new()));
        let save_events = Rc::clone(&events);
        let clear_events = Rc::clone(&events);
        let result = clear_snapshot_with(
            move |target, _| {
                save_events.borrow_mut().push(format!("save:{target}"));
                Ok(())
            },
            move |target| {
                if matches!(target, Z8_CREDENTIAL_ACCOUNT | Z8_LOGOUT_TOMBSTONE_TARGET) {
                    clear_events.borrow_mut().push(format!("clear:{target}"));
                }
                if target == Z8_CREDENTIAL_ACCOUNT {
                    Err(SecureStoreError::Backend { operation: "clear" })
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            result,
            Err(SecureStoreError::Backend { operation: "clear" })
        );
        let events = events.borrow();
        assert_eq!(
            events.first().map(String::as_str),
            Some(format!("save:{Z8_LOGOUT_TOMBSTONE_TARGET}").as_str())
        );
        assert!(events
            .iter()
            .any(|event| event == &format!("clear:{Z8_CREDENTIAL_ACCOUNT}")));
        assert!(!events
            .iter()
            .any(|event| event == &format!("clear:{Z8_LOGOUT_TOMBSTONE_TARGET}")));
    }

    #[test]
    fn failed_snapshot_write_never_clears_existing_tombstone() {
        let events = Rc::new(RefCell::new(Vec::<String>::new()));
        let save_events = Rc::clone(&events);
        let clear_events = Rc::clone(&events);
        let result = save_snapshot_with(
            &[("new-key".to_string(), "payload".to_string())],
            Z8_CREDENTIAL_ACCOUNT,
            "new-account",
            None,
            move |target, _| {
                save_events.borrow_mut().push(format!("save:{target}"));
                Err(SecureStoreError::Backend { operation: "save" })
            },
            move |target| {
                clear_events.borrow_mut().push(format!("clear:{target}"));
                Ok(())
            },
        );
        assert_eq!(result, Err(SecureStoreError::Backend { operation: "save" }));
        let events = events.borrow();
        assert_eq!(events.as_slice(), ["save:new-key"]);
    }

    #[test]
    fn snapshot_unmasks_only_after_account_record_is_durable() {
        let events = Rc::new(RefCell::new(Vec::<String>::new()));
        let save_events = Rc::clone(&events);
        let clear_events = Rc::clone(&events);
        let result = save_snapshot_with(
            &[("new-key".to_string(), "payload".to_string())],
            Z8_CREDENTIAL_ACCOUNT,
            "new-account",
            None,
            move |target, _| {
                save_events.borrow_mut().push(format!("save:{target}"));
                Ok(())
            },
            move |target| {
                if target == Z8_LOGOUT_TOMBSTONE_TARGET {
                    clear_events.borrow_mut().push(format!("clear:{target}"));
                }
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
        let events = events.borrow();
        let account = events
            .iter()
            .position(|event| event == &format!("save:{Z8_CREDENTIAL_ACCOUNT}"))
            .expect("account record should be written");
        let tombstone = events
            .iter()
            .position(|event| event == &format!("clear:{Z8_LOGOUT_TOMBSTONE_TARGET}"))
            .expect("tombstone should be removed");
        assert!(account < tombstone);
    }

    #[test]
    fn tombstone_clear_failure_returns_error_and_stays_fail_closed() {
        let events = Rc::new(RefCell::new(Vec::<String>::new()));
        let save_events = Rc::clone(&events);
        let clear_events = Rc::clone(&events);
        let result = save_snapshot_with(
            &[("new-key".to_string(), "payload".to_string())],
            Z8_CREDENTIAL_ACCOUNT,
            "new-account",
            None,
            move |target, _| {
                save_events.borrow_mut().push(format!("save:{target}"));
                Ok(())
            },
            move |target| {
                if target == Z8_LOGOUT_TOMBSTONE_TARGET {
                    clear_events.borrow_mut().push(format!("clear:{target}"));
                }
                if target == Z8_LOGOUT_TOMBSTONE_TARGET {
                    Err(SecureStoreError::Backend { operation: "clear" })
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            result,
            Err(SecureStoreError::Backend { operation: "clear" })
        );
        let events = events.borrow();
        assert!(events
            .iter()
            .any(|event| event == &format!("save:{Z8_CREDENTIAL_ACCOUNT}")));
        assert_eq!(
            events.last().map(String::as_str),
            Some(format!("clear:{Z8_LOGOUT_TOMBSTONE_TARGET}").as_str())
        );
    }

    #[test]
    fn oversized_single_credential_is_rejected_before_platform_write() {
        let payload = json!({"value": "x".repeat(MAX_CREDENTIAL_BLOB_BYTES + 1)});
        assert_eq!(
            bounded_json(&payload),
            Err(SecureStoreError::Backend {
                operation: "save (credential too large)"
            })
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_credential_parser_rejects_null_and_zero_length_blobs() {
        use std::ptr::null_mut;
        use windows::Win32::Security::Credentials::CREDENTIALW;

        let null_credential: *mut CREDENTIALW = null_mut();
        assert_eq!(
            unsafe { platform::credential_blob_to_string(null_credential) },
            Err(SecureStoreError::InvalidPayload)
        );

        let mut empty = CREDENTIALW {
            CredentialBlobSize: 0,
            CredentialBlob: null_mut(),
            ..Default::default()
        };
        assert_eq!(
            unsafe { platform::credential_blob_to_string(&mut empty) },
            Err(SecureStoreError::InvalidPayload)
        );

        let mut bytes = vec![b'x'; 2561];
        let mut too_large = CREDENTIALW {
            CredentialBlobSize: 2561,
            CredentialBlob: bytes.as_mut_ptr(),
            ..Default::default()
        };
        assert_eq!(
            unsafe { platform::credential_blob_to_string(&mut too_large) },
            Err(SecureStoreError::InvalidPayload)
        );
    }
}
