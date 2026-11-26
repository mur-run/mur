//! `RefusalCode` and `SecretLabel`: closed enums, so a refusal carries no
//! reply-chosen text (spec v3.6 to v3.9).

use serde::{Deserialize, Serialize};

/// What a secret finding is called on the wire. The daemon maps the scanner's
/// rule id to one of these, so a change in the scanner cannot change the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretLabel {
    OpenaiKey,
    AnthropicKey,
    AwsAccessKey,
    AwsSecretKey,
    GithubPat,
    GithubAppToken,
    GcpApiKey,
    Jwt,
    PemPrivateKey,
    SlackWebhook,
    EnvAssignment,
    /// A scanner label this crate does not know.
    Other,
}

impl SecretLabel {
    /// Every label, for round-trip tests and the parser.
    pub const ALL: [SecretLabel; 12] = [
        Self::OpenaiKey,
        Self::AnthropicKey,
        Self::AwsAccessKey,
        Self::AwsSecretKey,
        Self::GithubPat,
        Self::GithubAppToken,
        Self::GcpApiKey,
        Self::Jwt,
        Self::PemPrivateKey,
        Self::SlackWebhook,
        Self::EnvAssignment,
        Self::Other,
    ];

    /// Map a `SecretFinding.label` to a wire label; unknown becomes `Other`.
    pub fn from_scanner(label: &str) -> Self {
        Self::from_wire(label).unwrap_or(Self::Other)
    }

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiKey => "openai_key",
            Self::AnthropicKey => "anthropic_key",
            Self::AwsAccessKey => "aws_access_key",
            Self::AwsSecretKey => "aws_secret_key",
            Self::GithubPat => "github_pat",
            Self::GithubAppToken => "github_app_token",
            Self::GcpApiKey => "gcp_api_key",
            Self::Jwt => "jwt",
            Self::PemPrivateKey => "pem_private_key",
            Self::SlackWebhook => "slack_webhook",
            Self::EnvAssignment => "env_assignment",
            Self::Other => "other",
        }
    }

    /// Parse a wire spelling; `None` for anything not in [`Self::ALL`].
    pub fn from_wire(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == text)
    }
}

/// Why a request was refused. Wire form is a string: the variant name in
/// snake_case, and `secret_found:<label>` for the one with a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum RefusalCode {
    SecretFound(SecretLabel),
    StaleSeq,
    QuotaMember,
    QuotaFleet,
    QuotaTotal,
    StoreBusy,
    NotUtf8,
    ControlChar,
    Archive,
    TooLarge,
    NotMember,
    FleetStopped,
    BadSignature,
    NotFound,
    RefMismatch,
    Expired,
}

impl RefusalCode {
    /// One of each variant (with `Other` as the label), for tests.
    pub const SAMPLES: [RefusalCode; 16] = [
        Self::SecretFound(SecretLabel::Other),
        Self::StaleSeq,
        Self::QuotaMember,
        Self::QuotaFleet,
        Self::QuotaTotal,
        Self::StoreBusy,
        Self::NotUtf8,
        Self::ControlChar,
        Self::Archive,
        Self::TooLarge,
        Self::NotMember,
        Self::FleetStopped,
        Self::BadSignature,
        Self::NotFound,
        Self::RefMismatch,
        Self::Expired,
    ];

    /// The text shown to the model, from the variant alone.
    pub fn render(&self) -> String {
        match self {
            Self::SecretFound(label) => format!(
                "the draft was refused: it looks like it contains a secret ({}); rewrite it without the secret",
                label.as_str()
            ),
            Self::StaleSeq => {
                "the reference is out of date: the name has a newer version".to_owned()
            }
            Self::QuotaMember => "the member's shared-area quota is full".to_owned(),
            Self::QuotaFleet => "the fleet's shared-area quota is full".to_owned(),
            Self::QuotaTotal => "the shared area is full".to_owned(),
            Self::StoreBusy => "the shared area is busy; try again in a minute".to_owned(),
            Self::NotUtf8 => "the draft is not valid UTF-8 text".to_owned(),
            Self::ControlChar => "the draft has a control character that is not allowed".to_owned(),
            Self::Archive => {
                "the draft is an archive, which the shared area does not store".to_owned()
            }
            Self::TooLarge => "the draft is larger than the shared-area size limit".to_owned(),
            Self::NotMember => "the member is not part of that fleet".to_owned(),
            Self::FleetStopped => "the fleet is stopped".to_owned(),
            Self::BadSignature => "the reference signature does not verify".to_owned(),
            Self::NotFound => "there is no such shared object".to_owned(),
            Self::RefMismatch => "the reference does not match the stored admission".to_owned(),
            Self::Expired => "the shared object has expired".to_owned(),
        }
    }
}

/// Wire name of the one variant with a parameter.
const SECRET_FOUND: &str = "secret_found";
const SECRET_FOUND_PREFIX: &str = "secret_found:";

/// A wire string that is not a known code or carries a bad parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown refusal code")]
pub struct UnknownRefusalCode;

impl From<RefusalCode> for String {
    fn from(code: RefusalCode) -> String {
        match code {
            RefusalCode::SecretFound(label) => format!("{SECRET_FOUND}:{}", label.as_str()),
            RefusalCode::StaleSeq => "stale_seq".to_owned(),
            RefusalCode::QuotaMember => "quota_member".to_owned(),
            RefusalCode::QuotaFleet => "quota_fleet".to_owned(),
            RefusalCode::QuotaTotal => "quota_total".to_owned(),
            RefusalCode::StoreBusy => "store_busy".to_owned(),
            RefusalCode::NotUtf8 => "not_utf8".to_owned(),
            RefusalCode::ControlChar => "control_char".to_owned(),
            RefusalCode::Archive => "archive".to_owned(),
            RefusalCode::TooLarge => "too_large".to_owned(),
            RefusalCode::NotMember => "not_member".to_owned(),
            RefusalCode::FleetStopped => "fleet_stopped".to_owned(),
            RefusalCode::BadSignature => "bad_signature".to_owned(),
            RefusalCode::NotFound => "not_found".to_owned(),
            RefusalCode::RefMismatch => "ref_mismatch".to_owned(),
            RefusalCode::Expired => "expired".to_owned(),
        }
    }
}

impl TryFrom<String> for RefusalCode {
    type Error = UnknownRefusalCode;

    fn try_from(text: String) -> Result<Self, UnknownRefusalCode> {
        if let Some(label) = text.strip_prefix(SECRET_FOUND_PREFIX) {
            return SecretLabel::from_wire(label)
                .map(RefusalCode::SecretFound)
                .ok_or(UnknownRefusalCode);
        }
        Self::SAMPLES
            .into_iter()
            .find(|c| String::from(*c) == text)
            .ok_or(UnknownRefusalCode)
    }
}
