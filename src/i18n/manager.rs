use std::borrow::Cow;
use std::collections::HashMap;

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};

use super::{Locale, TextKey};

const EN_US: &str = include_str!("../../assets/locales/en-US/app.ftl");
const KO_KR: &str = include_str!("../../assets/locales/ko-KR/app.ftl");

pub struct LocalizationManager {
    locale: Locale,
    bundles: HashMap<Locale, FluentBundle<FluentResource>>,
}

impl LocalizationManager {
    pub fn new(locale: Locale) -> Self {
        Self {
            locale,
            bundles: HashMap::from([
                (Locale::EnUs, build_bundle(Locale::EnUs, EN_US)),
                (Locale::KoKr, build_bundle(Locale::KoKr, KO_KR)),
            ]),
        }
    }

    pub fn locale(&self) -> Locale {
        self.locale
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    pub fn text(&self, key: TextKey) -> Cow<'_, str> {
        self.format(key, None)
    }

    pub fn text_with<'a>(&'a self, key: TextKey, args: &'a FluentArgs<'a>) -> Cow<'a, str> {
        self.format(key, Some(args))
    }

    fn format<'a>(&'a self, key: TextKey, args: Option<&'a FluentArgs<'a>>) -> Cow<'a, str> {
        format_from(self.bundles.get(&self.locale), key, args)
            .or_else(|| format_from(self.bundles.get(&Locale::EnUs), key, args))
            .unwrap_or_else(|| Cow::Borrowed(key.id()))
    }
}

fn build_bundle(locale: Locale, source: &str) -> FluentBundle<FluentResource> {
    let resource = FluentResource::try_new(source.to_owned()).unwrap_or_else(|(_, errors)| {
        panic!("invalid {} language pack: {errors:?}", locale.as_str())
    });
    let mut bundle = FluentBundle::new(vec![locale.language_identifier()]);
    bundle.set_use_isolating(false);
    bundle
        .add_resource(resource)
        .unwrap_or_else(|errors| panic!("duplicate {} translations: {errors:?}", locale.as_str()));
    bundle
}

fn format_from<'a>(
    bundle: Option<&'a FluentBundle<FluentResource>>,
    key: TextKey,
    args: Option<&'a FluentArgs<'a>>,
) -> Option<Cow<'a, str>> {
    let bundle = bundle?;
    let message = bundle.get_message(key.id())?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    Some(bundle.format_pattern(pattern, args, &mut errors))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key, in every locale this ships.
    ///
    /// # Why this asks the bundles rather than `text`
    ///
    /// A missing translation never fails at runtime, and it does not even
    /// look wrong: [`LocalizationManager::format`] falls back to English
    /// before it falls back to the identifier, so a line forgotten in the
    /// Korean pack shows the English one. That is the right behaviour to
    /// ship — an English label beats `properties-font-size` in the middle of
    /// a dock — and it is exactly what makes the gap invisible to anyone
    /// running the application, and to a test that goes through `text`.
    ///
    /// So this reaches past the fallback and asks each bundle what it
    /// actually holds. It is the only thing standing between a forgotten
    /// line in one of two `.ftl` files and a release that is half in the
    /// wrong language.
    #[test]
    fn every_key_is_translated_in_every_locale() {
        let i18n = LocalizationManager::new(Locale::default());
        for locale in Locale::ALL {
            let bundle = i18n.bundles.get(&locale);
            let untranslated: Vec<&str> = TextKey::ALL
                .iter()
                .filter(|key| format_from(bundle, **key, None).is_none())
                .map(|key| key.id())
                .collect();
            assert!(
                untranslated.is_empty(),
                "the {} language pack is missing {}: {untranslated:?}",
                locale.as_str(),
                untranslated.len()
            );
        }
    }

    /// And nothing the packs hold is unreachable — a key that was renamed
    /// leaves its old line behind, which then reads as a translation that
    /// exists for something that no longer asks for one.
    #[test]
    fn no_language_pack_carries_a_line_nothing_asks_for() {
        let known: std::collections::HashSet<&str> =
            TextKey::ALL.iter().map(|key| key.id()).collect();
        for (locale, source) in [(Locale::EnUs, EN_US), (Locale::KoKr, KO_KR)] {
            let orphaned: Vec<&str> = source
                .lines()
                .filter_map(|line| line.split_once(" =").map(|(id, _)| id))
                .filter(|id| !id.starts_with('#') && !known.contains(id))
                .collect();
            assert!(
                orphaned.is_empty(),
                "the {} language pack translates {orphaned:?}, which no key asks for",
                locale.as_str()
            );
        }
    }

    #[test]
    fn locale_switches_text_and_formats_arguments() {
        let mut i18n = LocalizationManager::new(Locale::EnUs);
        assert_eq!(i18n.text(TextKey::MenuFile), "File");

        i18n.set_locale(Locale::KoKr);
        assert_eq!(i18n.text(TextKey::MenuFile), "파일");

        let mut args = FluentArgs::new();
        args.set("scene", "Scene 1");
        assert_eq!(
            i18n.text_with(TextKey::SourceEmpty, &args),
            "Scene 1에 소스가 없습니다"
        );
    }
}
