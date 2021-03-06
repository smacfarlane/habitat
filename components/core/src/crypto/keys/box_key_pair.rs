use super::{super::{ANONYMOUS_BOX_FORMAT_VERSION,
                    BOX_FORMAT_VERSION,
                    PUBLIC_BOX_KEY_VERSION,
                    PUBLIC_KEY_SUFFIX,
                    SECRET_BOX_KEY_SUFFIX,
                    SECRET_BOX_KEY_VERSION},
            get_key_revisions,
            mk_key_filename,
            mk_revision_string,
            parse_name_with_rev,
            read_key_bytes,
            read_key_bytes_from_str,
            write_keypair_files,
            KeyPair,
            KeyType};
use crate::error::{Error,
                   Result};
use base64;
use serde_derive::{Deserialize,
                   Serialize};
use sodiumoxide::crypto::{box_::{self,
                                 curve25519xsalsa20poly1305::{gen_nonce,
                                                              Nonce,
                                                              PublicKey as BoxPublicKey,
                                                              SecretKey as BoxSecretKey}},
                          sealedbox};
use std::{borrow::Cow,
          path::{Path,
                 PathBuf},
          str};

#[derive(Debug)]
pub struct BoxSecret<'a> {
    pub sender:     &'a str,
    pub ciphertext: Vec<u8>,
    pub receiver:   Option<&'a str>,
    pub nonce:      Option<Nonce>,
}

pub type BoxKeyPair = KeyPair<BoxPublicKey, BoxSecretKey>;

// A sodiumoxide sealed box that has been base64-encoded together with
// metadata to indicate how it should be decrypted
#[derive(Serialize, Deserialize)]
pub struct WrappedSealedBox<'a>(Cow<'a, str>);

impl<'a> WrappedSealedBox<'a> {
    pub fn into_bytes(self) -> Vec<u8> { self.0.into_owned().into_bytes() }

    /// Only needed by builder due to double-base64 encoding in
    /// builder_core::integrations::encrypt
    pub fn as_bytes(&self) -> &[u8] { self.0.as_bytes() }

    pub fn from_bytes(bytes: &'a [u8]) -> std::result::Result<Self, std::str::Utf8Error> {
        str::from_utf8(bytes).map(Cow::Borrowed)
                             .map(WrappedSealedBox)
    }
}

impl<'a> From<String> for WrappedSealedBox<'a> {
    fn from(payload: String) -> Self { Self(Cow::Owned(payload)) }
}

impl<'a, 'b: 'a> From<&'b str> for WrappedSealedBox<'a> {
    fn from(payload: &'b str) -> Self { Self(Cow::Borrowed(payload)) }
}

impl BoxKeyPair {
    pub fn generate_pair_for_service<S1, S2>(org: S1, service_group: S2) -> Result<Self>
        where S1: AsRef<str>,
              S2: AsRef<str>
    {
        let revision = mk_revision_string()?;
        let keyname =
            Self::mk_key_name_for_service(org.as_ref(), service_group.as_ref(), &revision);
        debug!("new service box key name = {}", &keyname);
        let (pk, sk) = box_::gen_keypair();
        let (name, _) = parse_name_with_rev(&keyname)?;
        Ok(Self::new(name, revision, Some(pk), Some(sk)))
    }

    pub fn generate_pair_for_user(user: &str) -> Result<Self> {
        debug!("new user box key");
        Self::generate_pair_for_string(user)
    }

    pub fn generate_pair_for_origin(origin: &str) -> Result<Self> {
        debug!("new origin box key");
        Self::generate_pair_for_string(origin)
    }

    pub fn get_pairs_for<T, P>(name: T, cache_key_path: P) -> Result<Vec<Self>>
        where T: AsRef<str>,
              P: AsRef<Path>
    {
        let revisions =
            get_key_revisions(name.as_ref(), cache_key_path.as_ref(), None, &KeyType::Box)?;
        let mut key_pairs = Vec::new();
        for name_with_rev in revisions {
            debug!("Attempting to read key name_with_rev {} for {}",
                   name_with_rev,
                   name.as_ref());
            let kp = Self::get_pair_for(name_with_rev, cache_key_path.as_ref())?;
            key_pairs.push(kp);
        }
        Ok(key_pairs)
    }

    pub fn get_pair_for<T, P>(name_with_rev: T, cache_key_path: P) -> Result<Self>
        where T: AsRef<str>,
              P: AsRef<Path>
    {
        let (name, rev) = parse_name_with_rev(name_with_rev.as_ref())?;
        let pk = match Self::get_public_key(name_with_rev.as_ref(), cache_key_path.as_ref()) {
            Ok(k) => Some(k),
            Err(e) => {
                debug!("Can't find public key for name_with_rev {}: {}",
                       name_with_rev.as_ref(),
                       e);
                None
            }
        };
        let sk = match Self::get_secret_key(name_with_rev.as_ref(), cache_key_path.as_ref()) {
            Ok(k) => Some(k),
            Err(e) => {
                debug!("Can't find secret key for name_with_rev {}: {}",
                       name_with_rev.as_ref(),
                       e);
                None
            }
        };
        if pk == None && sk == None {
            let msg = format!("No public or secret keys found for name_with_rev {}",
                              name_with_rev.as_ref());
            return Err(Error::CryptoError(msg));
        }
        Ok(Self::new(name, rev, pk, sk))
    }

    pub fn get_latest_pair_for<T, P>(name: T, cache_key_path: P) -> Result<Self>
        where T: AsRef<str>,
              P: AsRef<Path>
    {
        let mut all = Self::get_pairs_for(name.as_ref(), cache_key_path.as_ref())?;
        match all.len() {
            0 => {
                let msg = format!("No revisions found for {} box key", name.as_ref());
                Err(Error::CryptoError(msg))
            }
            _ => Ok(all.remove(0)),
        }
    }

    pub fn get_public_key_path<P: AsRef<Path> + ?Sized>(key_with_rev: &str,
                                                        cache_key_path: &P)
                                                        -> Result<PathBuf> {
        let path = mk_key_filename(cache_key_path.as_ref(), key_with_rev, PUBLIC_KEY_SUFFIX);
        if !path.is_file() {
            return Err(Error::CryptoError(format!("No public key found at {}", path.display())));
        }
        Ok(path)
    }

    pub fn get_secret_key_path<P: AsRef<Path> + ?Sized>(key_with_rev: &str,
                                                        cache_key_path: &P)
                                                        -> Result<PathBuf> {
        let path = mk_key_filename(cache_key_path.as_ref(), key_with_rev, SECRET_BOX_KEY_SUFFIX);
        if !path.is_file() {
            return Err(Error::CryptoError(format!("No secret key found at {}", path.display())));
        }
        Ok(path)
    }

    /// A user can encrypt data with a service as the recipient.
    /// Key names and nonce (if needed) are embedded in the payload.
    /// If no recipient is specified, the encrypted payload is decryptable only
    /// by the encrypting user.
    ///
    /// Since the returned string contains both plaintext metadata and ciphertext
    /// The ciphertext (and nonce, when present) is already base64-encoded.
    pub fn encrypt(&self, data: &[u8], receiver: Option<&Self>) -> Result<WrappedSealedBox> {
        match receiver {
            Some(r) => self.encrypt_box(data, r),
            None => self.encrypt_anonymous_box(data),
        }.map(WrappedSealedBox::from)
    }

    pub fn to_public_string(&self) -> Result<String> {
        match self.public {
            Some(pk) => {
                Ok(format!("{}\n{}\n\n{}",
                           PUBLIC_BOX_KEY_VERSION,
                           self.name_with_rev(),
                           &base64::encode(&pk[..])))
            }
            None => {
                Err(Error::CryptoError(format!("No public key present for {}",
                                               self.name_with_rev())))
            }
        }
    }

    pub fn to_secret_string(&self) -> Result<String> {
        match self.secret {
            Some(ref sk) => {
                Ok(format!("{}\n{}\n\n{}",
                           SECRET_BOX_KEY_VERSION,
                           self.name_with_rev(),
                           &base64::encode(&sk[..])))
            }
            None => {
                Err(Error::CryptoError(format!("No secret key present for {}",
                                               self.name_with_rev())))
            }
        }
    }

    fn generate_pair_for_string(string: &str) -> Result<Self> {
        let revision = mk_revision_string()?;
        let keyname = Self::mk_key_name_for_string(string, &revision);
        debug!("new sig key name = {}", &keyname);
        let (pk, sk) = box_::gen_keypair();
        let (name, _) = parse_name_with_rev(&keyname)?;
        Ok(Self::new(name, revision, Some(pk), Some(sk)))
    }

    fn encrypt_box(&self, data: &[u8], receiver: &Self) -> Result<String> {
        let nonce = gen_nonce();
        let ciphertext = box_::seal(data, &nonce, receiver.public()?, self.secret()?);

        Ok(format!("{}\n{}\n{}\n{}\n{}",
                   BOX_FORMAT_VERSION,
                   &self.name_with_rev(),
                   &receiver.name_with_rev(),
                   base64::encode(&nonce[..]),
                   base64::encode(&ciphertext)))
    }

    fn encrypt_anonymous_box(&self, data: &[u8]) -> Result<String> {
        let ciphertext = sealedbox::seal(data, self.public()?);

        Ok(format!("{}\n{}\n{}",
                   ANONYMOUS_BOX_FORMAT_VERSION,
                   &self.name_with_rev(),
                   base64::encode(&ciphertext)))
    }

    pub fn box_key_format_version(version: Option<&str>) -> Result<&str> {
        match version {
            Some(val) => {
                if val != BOX_FORMAT_VERSION && val != ANONYMOUS_BOX_FORMAT_VERSION {
                    return Err(Error::CryptoError(format!("Unsupported version: {}", val)));
                };
                Ok(val)
            }
            None => Err(Error::CryptoError("Corrupt payload, can't read version".to_string())),
        }
    }

    pub fn box_key_sender(sender: Option<&str>) -> Result<&str> {
        match sender {
            Some(val) => Ok(val),
            None => {
                Err(Error::CryptoError("Corrupt payload, can't read sender \
                                        key name"
                                                 .to_string()))
            }
        }
    }

    pub fn box_key_receiver(receiver: Option<&str>) -> Result<&str> {
        match receiver {
            Some(val) => Ok(val),
            None => {
                Err(Error::CryptoError("Corrupt payload, can't read receiver \
                                        key name"
                                                 .to_string()))
            }
        }
    }

    pub fn box_key_nonce(nonce: Option<&str>) -> Result<Nonce> {
        match nonce {
            Some(val) => {
                let decoded =
                    base64::decode(val).map_err(|e| {
                                           Error::CryptoError(format!("Can't decode nonce: {}", e))
                                       })?;
                match Nonce::from_slice(&decoded) {
                    Some(nonce) => Ok(nonce),
                    None => Err(Error::CryptoError("Invalid size of nonce".to_string())),
                }
            }
            None => Err(Error::CryptoError("Corrupt payload, can't read nonce".to_string())),
        }
    }

    pub fn box_key_ciphertext(ciphertext: Option<&str>) -> Result<Vec<u8>> {
        match ciphertext {
            Some(val) => {
                Ok(base64::decode(val).map_err(|e| {
                                          Error::CryptoError(format!("Can't decode ciphertext: {}",
                                                                     e))
                                      })?)
            }
            None => Err(Error::CryptoError("Corrupt payload, can't read ciphertext".to_string())),
        }
    }

    pub fn is_anonymous_box(version: &str) -> bool { version == ANONYMOUS_BOX_FORMAT_VERSION }

    pub fn decrypt(&self,
                   ciphertext: &[u8],
                   receiver: Option<Self>,
                   nonce: Option<Nonce>)
                   -> Result<Vec<u8>> {
        match receiver {
            Some(recv) => {
                Self::decrypt_box(ciphertext, &nonce.unwrap(), self.public()?, recv.secret()?)
            }
            None => Self::decrypt_anonymous_box(ciphertext, self.public()?, self.secret()?),
        }
    }

    // Return the metadata and encrypted text from a secret payload.
    // This is useful for services consuming an encrypted payload and need to decrypt it without
    // having keys on disk
    pub fn secret_metadata<'a, 'b>(payload: &'b WrappedSealedBox<'a>) -> Result<BoxSecret<'b>> {
        let mut lines = payload.0.lines();
        let version = Self::box_key_format_version(lines.next())?;
        let sender = Self::box_key_sender(lines.next())?;
        let receiver = if Self::is_anonymous_box(version) {
            None
        } else {
            Some(Self::box_key_receiver(lines.next())?)
        };
        let nonce = if Self::is_anonymous_box(version) {
            None
        } else {
            Some(Self::box_key_nonce(lines.next())?)
        };
        let ciphertext = Self::box_key_ciphertext(lines.next())?;
        Ok(BoxSecret { sender,
                       receiver,
                       nonce,
                       ciphertext })
    }

    /// Decrypt data from a user that was received at a service
    /// Key names are embedded in the message payload which must
    /// be present while decrypting.
    pub fn decrypt_with_path<P>(payload: &WrappedSealedBox, cache_key_path: P) -> Result<Vec<u8>>
        where P: AsRef<Path>
    {
        debug!("Decrypt key path = {}", cache_key_path.as_ref().display());
        let box_secret = Self::secret_metadata(payload)?;
        let sender = Self::get_pair_for(box_secret.sender, cache_key_path.as_ref())?;
        let receiver = match box_secret.receiver {
            Some(recv) => Some(Self::get_pair_for(recv, cache_key_path.as_ref())?),
            None => None,
        };
        sender.decrypt(&box_secret.ciphertext, receiver, box_secret.nonce)
    }

    pub fn to_pair_files<P: AsRef<Path> + ?Sized>(&self, path: &P) -> Result<()> {
        let public_keyfile = mk_key_filename(path, self.name_with_rev(), PUBLIC_KEY_SUFFIX);
        let secret_keyfile = mk_key_filename(path, self.name_with_rev(), SECRET_BOX_KEY_SUFFIX);
        debug!("public sig keyfile = {}", public_keyfile.display());
        debug!("secret sig keyfile = {}", secret_keyfile.display());

        write_keypair_files(Some(&public_keyfile),
                            Some(self.to_public_string()?),
                            Some(&secret_keyfile),
                            Some(self.to_secret_string()?))
    }

    fn decrypt_box(ciphertext: &[u8],
                   nonce: &Nonce,
                   pk: &BoxPublicKey,
                   sk: &BoxSecretKey)
                   -> Result<Vec<u8>> {
        box_::open(ciphertext, nonce, pk, sk).map_err(|_| {
                                                 Error::CryptoError("Secret key, public key, and \
                                                                     nonce could not decrypt \
                                                                     ciphertext"
                                                                                .to_string())
                                             })
    }

    fn decrypt_anonymous_box(ciphertext: &[u8],
                             pk: &BoxPublicKey,
                             sk: &BoxSecretKey)
                             -> Result<Vec<u8>> {
        sealedbox::open(ciphertext, &pk, &sk).map_err(|_| {
            Error::CryptoError("Secret key and public key could not decrypt ciphertext".to_string())
        })
    }

    pub fn public_key_from_str(key: &str) -> Result<BoxPublicKey> {
        Self::public_key_from_bytes(&read_key_bytes_from_str(key)?)
    }

    pub fn public_key_from_bytes(bytes: &[u8]) -> Result<BoxPublicKey> {
        match BoxPublicKey::from_slice(bytes) {
            Some(sk) => Ok(sk),
            None => {
                Err(Error::CryptoError("Can't convert key bytes to \
                                        BoxPublicKey"
                                                     .to_string()))
            }
        }
    }

    fn get_public_key<T, P>(key_with_rev: T, cache_key_path: P) -> Result<BoxPublicKey>
        where T: AsRef<str>,
              P: AsRef<Path>
    {
        let public_keyfile =
            mk_key_filename(cache_key_path, key_with_rev.as_ref(), PUBLIC_KEY_SUFFIX);
        let bytes = read_key_bytes(&public_keyfile)?;
        Self::public_key_from_bytes(&bytes)
    }

    fn get_secret_key<T, P>(key_with_rev: T, cache_key_path: P) -> Result<BoxSecretKey>
        where T: AsRef<str>,
              P: AsRef<Path>
    {
        let secret_keyfile =
            mk_key_filename(cache_key_path, key_with_rev.as_ref(), SECRET_BOX_KEY_SUFFIX);
        let bytes = read_key_bytes(&secret_keyfile)?;
        Self::secret_key_from_bytes(&bytes)
    }

    pub fn secret_key_from_str(key: &str) -> Result<BoxSecretKey> {
        Self::secret_key_from_bytes(&read_key_bytes_from_str(key)?)
    }

    pub fn secret_key_from_bytes(bytes: &[u8]) -> Result<BoxSecretKey> {
        match BoxSecretKey::from_slice(bytes) {
            Some(sk) => Ok(sk),
            None => {
                Err(Error::CryptoError("Can't convert key bytes to \
                                        BoxSecretKey"
                                                     .to_string()))
            }
        }
    }

    fn mk_key_name_for_service(org: &str, service_group: &str, revision: &str) -> String {
        format!("{}@{}-{}", service_group, org, revision)
    }

    fn mk_key_name_for_string(string: &str, revision: &str) -> String {
        format!("{}-{}", string, revision)
    }
}

#[cfg(test)]
mod test {
    use std::{fs,
              str};

    use tempfile::Builder;

    use super::{super::super::test_support::*,
                BoxKeyPair,
                *};

    static VALID_KEY: &str = "service-key-valid.default@acme-20160509181736.box.key";
    static VALID_PUB: &str = "service-key-valid.default@acme-20160509181736.pub";
    static VALID_NAME_WITH_REV: &str = "service-key-valid.default@acme-20160509181736";

    #[test]
    fn empty_struct() {
        let pair = BoxKeyPair::new("grohl".to_string(), "201604051449".to_string(), None, None);

        assert_eq!(pair.name, "grohl");
        assert_eq!(pair.rev, "201604051449");
        assert_eq!(pair.name_with_rev(), "grohl-201604051449");

        assert_eq!(pair.public, None);
        if let Err(Error::CryptoError(_)) = pair.public() {
            // OK
        } else {
            panic!("Expected Error::CryptoError");
        }
        assert_eq!(pair.secret, None);
        assert!(pair.secret().is_err(),
                "Empty pair should not have a secret key");
    }

    #[test]
    fn generated_service_pair() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let pair = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        pair.to_pair_files(cache.path()).unwrap();

        assert_eq!(pair.name, "tnt.default@acme");
        assert!(pair.public().is_ok(),
                "Generated pair should have a public key");
        assert!(pair.secret().is_ok(),
                "Generated pair should have a secret key");
        assert!(cache.path()
                     .join(format!("{}.pub", pair.name_with_rev()))
                     .exists());
        assert!(cache.path()
                     .join(format!("{}.box.key", pair.name_with_rev()))
                     .exists());
    }

    #[test]
    fn generated_user_pair() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let pair = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        pair.to_pair_files(cache.path()).unwrap();

        assert_eq!(pair.name, "wecoyote");
        assert!(pair.public().is_ok(),
                "Generated pair should have a public key");
        assert!(pair.secret().is_ok(),
                "Generated pair should have a secret key");
        assert!(cache.path()
                     .join(format!("{}.pub", pair.name_with_rev()))
                     .exists());
        assert!(cache.path()
                     .join(format!("{}.box.key", pair.name_with_rev()))
                     .exists());
    }

    #[test]
    fn get_pairs_for() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let pairs = BoxKeyPair::get_pairs_for("wecoyote", cache.path()).unwrap();
        assert_eq!(pairs.len(), 0);

        BoxKeyPair::generate_pair_for_user("wecoyote").unwrap()
                                                      .to_pair_files(cache.path())
                                                      .unwrap();
        let pairs = BoxKeyPair::get_pairs_for("wecoyote", cache.path()).unwrap();
        assert_eq!(pairs.len(), 1);

        match wait_until_ok(|| {
                  let pair = BoxKeyPair::generate_pair_for_user("wecoyote")?;
                  pair.to_pair_files(cache.path())?;
                  Ok(())
              }) {
            Some(_) => (),
            None => panic!("Failed to generate another keypair after waiting"),
        };
        let pairs = BoxKeyPair::get_pairs_for("wecoyote", cache.path()).unwrap();
        assert_eq!(pairs.len(), 2);

        // We should not include another named key in the count
        BoxKeyPair::generate_pair_for_user("roadrunner").unwrap()
                                                        .to_pair_files(cache.path())
                                                        .unwrap();
        let pairs = BoxKeyPair::get_pairs_for("wecoyote", cache.path()).unwrap();
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn get_pair_for() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let p1 = BoxKeyPair::generate_pair_for_user("web").unwrap();
        p1.to_pair_files(cache.path()).unwrap();
        let p2 = match wait_until_ok(|| {
                  let upair = BoxKeyPair::generate_pair_for_user("web")?;
                  upair.to_pair_files(cache.path())?;
                  Ok(upair)
              }) {
            Some(pair) => pair,
            None => panic!("Failed to generate another keypair after waiting"),
        };

        let p1_fetched = BoxKeyPair::get_pair_for(&p1.name_with_rev(), cache.path()).unwrap();
        assert_eq!(p1.name, p1_fetched.name);
        assert_eq!(p1.rev, p1_fetched.rev);
        let p2_fetched = BoxKeyPair::get_pair_for(&p2.name_with_rev(), cache.path()).unwrap();
        assert_eq!(p2.name, p2_fetched.name);
        assert_eq!(p2.rev, p2_fetched.rev);
    }

    #[test]
    #[should_panic(expected = "No public or secret keys found for")]
    fn get_pair_for_nonexistent() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        BoxKeyPair::get_pair_for("nope-nope-20160405144901", cache.path()).unwrap();
    }

    #[test]
    fn get_latest_pair_for_single() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let pair = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        pair.to_pair_files(cache.path()).unwrap();

        let latest = BoxKeyPair::get_latest_pair_for("wecoyote", cache.path()).unwrap();
        assert_eq!(latest.name, pair.name);
        assert_eq!(latest.rev, pair.rev);
    }

    #[test]
    fn get_latest_pair_for_multiple() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        BoxKeyPair::generate_pair_for_user("web").unwrap()
                                                 .to_pair_files(cache.path())
                                                 .unwrap();
        let p2 = match wait_until_ok(|| {
                  let upair = BoxKeyPair::generate_pair_for_user("web")?;
                  upair.to_pair_files(cache.path())?;
                  Ok(upair)
              }) {
            Some(pair) => pair,
            None => panic!("Failed to generate another keypair after waiting"),
        };

        let latest = BoxKeyPair::get_latest_pair_for("web", cache.path()).unwrap();
        assert_eq!(latest.name, p2.name);
        assert_eq!(latest.rev, p2.rev);
    }

    #[test]
    #[should_panic(expected = "No revisions found for")]
    fn get_latest_pair_for_nonexistent() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        BoxKeyPair::get_latest_pair_for("nope-nope", cache.path()).unwrap();
    }

    #[test]
    fn get_public_key_path() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        fs::copy(fixture(&format!("keys/{}", VALID_PUB)),
                 cache.path().join(VALID_PUB)).unwrap();

        let result = BoxKeyPair::get_public_key_path(VALID_NAME_WITH_REV, cache.path()).unwrap();
        assert_eq!(result, cache.path().join(VALID_PUB));
    }

    #[test]
    #[should_panic(expected = "No public key found at")]
    fn get_public_key_path_nonexistent() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        BoxKeyPair::get_public_key_path(VALID_NAME_WITH_REV, cache.path()).unwrap();
    }

    #[test]
    fn get_secret_key_path() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        fs::copy(fixture(&format!("keys/{}", VALID_KEY)),
                 cache.path().join(VALID_KEY)).unwrap();

        let result = BoxKeyPair::get_secret_key_path(VALID_NAME_WITH_REV, cache.path()).unwrap();
        assert_eq!(result, cache.path().join(VALID_KEY));
    }

    #[test]
    #[should_panic(expected = "No secret key found at")]
    fn get_secret_key_path_nonexistent() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        BoxKeyPair::get_secret_key_path(VALID_NAME_WITH_REV, cache.path()).unwrap();
    }

    #[test]
    fn encrypt_and_decrypt_from_user_to_service() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let service = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        service.to_pair_files(cache.path()).unwrap();

        let user = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        user.to_pair_files(cache.path()).unwrap();

        let ciphertext = user.encrypt(b"I wish to buy more rockets", Some(&service))
                             .unwrap();
        let message = BoxKeyPair::decrypt_with_path(&ciphertext, cache.path()).unwrap();
        assert_eq!(message, b"I wish to buy more rockets");
    }

    #[test]
    fn encrypt_and_decrypt_from_service_to_user() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let service = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        service.to_pair_files(cache.path()).unwrap();
        let user = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        user.to_pair_files(cache.path()).unwrap();

        let ciphertext = service.encrypt(b"Out of rockets", Some(&user)).unwrap();
        let message = BoxKeyPair::decrypt_with_path(&ciphertext, cache.path()).unwrap();
        assert_eq!(message, b"Out of rockets");
    }

    #[test]
    fn encrypt_and_decrypt_to_self() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();

        let ciphertext = sender.encrypt(b"Buy more rockets", None).unwrap();
        let message = BoxKeyPair::decrypt_with_path(&ciphertext, cache.path()).unwrap();
        assert_eq!(message, b"Buy more rockets");
    }

    #[test]
    fn encrypt_to_self_with_only_public_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();

        // Delete the sender's secret key
        fs::remove_file(
            BoxKeyPair::get_secret_key_path(&sender.name_with_rev(), cache.path()).unwrap(),
        )
        .unwrap();
        // Now reload the sender's pair which will be missing the secret key
        let sender = BoxKeyPair::get_latest_pair_for("wecoyote", cache.path()).unwrap();

        let ciphertext = sender.encrypt(b"Nothing to see here", None);
        assert!(ciphertext.is_ok());
    }

    #[test]
    fn encrypt_and_decrypt_minimal_keys() {
        let full_cache = Builder::new().prefix("full_cache").tempdir().unwrap();
        let sender_cache = Builder::new().prefix("sender_cache").tempdir().unwrap();
        let receiver_cache = Builder::new().prefix("receiver_cache").tempdir().unwrap();

        // Generate the keys & prepare the sender and receiver caches with the minimal keys
        // required on each end
        {
            let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
            sender.to_pair_files(full_cache.path()).unwrap();
            let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
            receiver.to_pair_files(full_cache.path()).unwrap();

            // Prepare the sender cache with sender's secret and receiver's public keys
            let secret = BoxKeyPair::get_secret_key_path(&sender.name_with_rev(),
                                                         full_cache.path()).unwrap();
            let public = BoxKeyPair::get_public_key_path(&receiver.name_with_rev(),
                                                         full_cache.path()).unwrap();
            fs::copy(&secret,
                     sender_cache.path().join(&secret.file_name().unwrap())).unwrap();
            fs::copy(&public,
                     sender_cache.path().join(&public.file_name().unwrap())).unwrap();

            // Prepare the receiver cache with receivers's secret and sender's public keys
            let secret = BoxKeyPair::get_secret_key_path(&receiver.name_with_rev(),
                                                         full_cache.path()).unwrap();
            let public = BoxKeyPair::get_public_key_path(&sender.name_with_rev(),
                                                         full_cache.path()).unwrap();
            fs::copy(&secret,
                     receiver_cache.path().join(&secret.file_name().unwrap())).unwrap();
            fs::copy(&public,
                     receiver_cache.path().join(&public.file_name().unwrap())).unwrap();
        }

        let sender = BoxKeyPair::get_latest_pair_for("wecoyote", sender_cache.path()).unwrap();
        let ciphertext = {
            // Load the sender and receiver keys from sender cache to encrypt
            let receiver =
                BoxKeyPair::get_latest_pair_for("tnt.default@acme", sender_cache.path()).unwrap();
            sender.encrypt(b"Falling hurts", Some(&receiver)).unwrap()
        };

        // Decrypt unpacks the ciphertext payload to read nonce , determines which secret key to
        // load for the receiver and which public key to load for the sender. We're using the
        // receiver's cache for the decrypt.
        let message = BoxKeyPair::decrypt_with_path(&ciphertext, receiver_cache.path()).unwrap();
        assert_eq!(message, b"Falling hurts");
    }

    #[test]
    #[should_panic(expected = "Secret key is required but not present for")]
    fn encrypt_missing_sender_secret_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();
        let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        receiver.to_pair_files(cache.path()).unwrap();

        // Delete the sender's secret key
        fs::remove_file(
            BoxKeyPair::get_secret_key_path(&sender.name_with_rev(), cache.path()).unwrap(),
        )
        .unwrap();
        // Now reload the sender's pair which will be missing the secret key
        let sender = BoxKeyPair::get_latest_pair_for("wecoyote", cache.path()).unwrap();

        sender.encrypt(b"not going to happen", Some(&receiver))
              .unwrap();
    }

    #[test]
    #[should_panic(expected = "Public key is required but not present for")]
    fn encrypt_missing_receiver_public_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();
        let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        receiver.to_pair_files(cache.path()).unwrap();

        // Delete the receiver's public key
        fs::remove_file(
            BoxKeyPair::get_public_key_path(&receiver.name_with_rev(), cache.path()).unwrap(),
        )
        .unwrap();
        // Now reload the receiver's pair which will be missing the public key
        let receiver = BoxKeyPair::get_latest_pair_for("tnt.default@acme", cache.path()).unwrap();

        sender.encrypt(b"not going to happen", Some(&receiver))
              .unwrap();
    }

    #[test]
    #[should_panic(expected = "Secret key is required but not present for")]
    fn decrypt_missing_receiver_secret_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();
        let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        receiver.to_pair_files(cache.path()).unwrap();

        // Delete the receiver's secret key
        fs::remove_file(
            BoxKeyPair::get_secret_key_path(&receiver.name_with_rev(), cache.path()).unwrap(),
        )
        .unwrap();

        let ciphertext = sender.encrypt(b"problems ahead", Some(&receiver)).unwrap();
        BoxKeyPair::decrypt_with_path(&ciphertext, cache.path()).unwrap();
    }

    #[test]
    #[should_panic(expected = "Public key is required but not present for")]
    fn decrypt_missing_sender_public_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();
        let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        receiver.to_pair_files(cache.path()).unwrap();

        // Delete the sender's public key
        fs::remove_file(
            BoxKeyPair::get_public_key_path(&sender.name_with_rev(), cache.path()).unwrap(),
        )
        .unwrap();

        let ciphertext = sender.encrypt(b"problems ahead", Some(&receiver)).unwrap();
        BoxKeyPair::decrypt_with_path(&ciphertext, cache.path()).unwrap();
    }

    #[test]
    #[should_panic]
    fn decrypt_empty_sender_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        BoxKeyPair::decrypt_with_path(&WrappedSealedBox::from("BOX-1\n\nuhoh"), cache.path())
            .unwrap();
    }

    #[test]
    #[should_panic]
    fn decrypt_invalid_sender_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        BoxKeyPair::decrypt_with_path(&WrappedSealedBox::from("BOX-1\nnope-nope\nuhoh"),
                                      cache.path()).unwrap();
    }

    #[test]
    #[should_panic]
    fn decrypt_empty_receiver_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();

        let payload = format!("BOX-1\n{}\n\nuhoh", sender.name_with_rev());
        BoxKeyPair::decrypt_with_path(&WrappedSealedBox::from(payload), cache.path()).unwrap();
    }

    #[test]
    #[should_panic]
    fn decrypt_invalid_receiver_key() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();

        let payload = format!("BOX-1\n{}\nnope-nope\nuhoh", sender.name_with_rev());
        BoxKeyPair::decrypt_with_path(&WrappedSealedBox::from(payload), cache.path()).unwrap();
    }

    #[test]
    #[should_panic(expected = "Can\\'t decode nonce")]
    fn decrypt_invalid_nonce_decode() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();
        let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        receiver.to_pair_files(cache.path()).unwrap();

        let payload = format!("BOX-1\n{}\n{}\nnot:base64",
                              sender.name_with_rev(),
                              receiver.name_with_rev());
        BoxKeyPair::decrypt_with_path(&WrappedSealedBox::from(payload), cache.path()).unwrap();
    }

    #[test]
    #[should_panic(expected = "Invalid size of nonce")]
    fn decrypt_invalid_nonce() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();
        let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        receiver.to_pair_files(cache.path()).unwrap();

        let payload = format!("BOX-1\n{}\n{}\nuhoh",
                              sender.name_with_rev(),
                              receiver.name_with_rev());
        BoxKeyPair::decrypt_with_path(&WrappedSealedBox::from(payload), cache.path()).unwrap();
    }

    #[test]
    #[should_panic(expected = "Can\\'t decode ciphertext")]
    fn decrypt_invalid_ciphertext_decode() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();
        let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        receiver.to_pair_files(cache.path()).unwrap();

        let payload = sender.encrypt(b"problems ahead", Some(&receiver)).unwrap();
        let mut botched = String::new();
        let mut lines = payload.0.lines();
        botched.push_str(lines.next().unwrap()); // version
        botched.push('\n');
        botched.push_str(lines.next().unwrap()); // sender
        botched.push('\n');
        botched.push_str(lines.next().unwrap()); // receiver
        botched.push('\n');
        botched.push_str(lines.next().unwrap()); // nonce
        botched.push('\n');
        botched.push_str("not:base64");

        BoxKeyPair::decrypt_with_path(&WrappedSealedBox::from(botched), cache.path()).unwrap();
    }

    #[test]
    #[should_panic(expected = "Secret key, public key, and nonce could not decrypt ciphertext")]
    fn decrypt_invalid_ciphertext() {
        let cache = Builder::new().prefix("key_cache").tempdir().unwrap();
        let sender = BoxKeyPair::generate_pair_for_user("wecoyote").unwrap();
        sender.to_pair_files(cache.path()).unwrap();
        let receiver = BoxKeyPair::generate_pair_for_service("acme", "tnt.default").unwrap();
        receiver.to_pair_files(cache.path()).unwrap();

        let payload = sender.encrypt(b"problems ahead", Some(&receiver)).unwrap();
        let mut botched = String::new();
        let mut lines = payload.0.lines();
        botched.push_str(lines.next().unwrap()); // version
        botched.push('\n');
        botched.push_str(lines.next().unwrap()); // sender
        botched.push('\n');
        botched.push_str(lines.next().unwrap()); // receiver
        botched.push('\n');
        botched.push_str(lines.next().unwrap()); // nonce
        botched.push('\n');
        botched.push_str("uhoh");

        BoxKeyPair::decrypt_with_path(&WrappedSealedBox::from(botched), cache.path()).unwrap();
    }
}
