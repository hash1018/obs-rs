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
