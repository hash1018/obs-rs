use serde::{Deserialize, Serialize};
use unic_langid::LanguageIdentifier;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Locale {
    #[serde(rename = "en-US")]
    #[default]
    EnUs,
    #[serde(rename = "ko-KR")]
    KoKr,
}

impl Locale {
    pub const ALL: [Self; 2] = [Self::EnUs, Self::KoKr];

    pub fn language_identifier(self) -> LanguageIdentifier {
        self.as_str()
            .parse()
            .expect("built-in locale identifiers must be valid")
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnUs => "en-US",
            Self::KoKr => "ko-KR",
        }
    }
}
