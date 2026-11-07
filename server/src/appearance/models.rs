//! The appearance routes' wire shapes.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use super::ThemeChoice;

/// Everything the browser needs to paint the caller's pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MyAppearance {
    /// The theme the caller chose, or null while they chose none.
    pub theme: Option<ThemeChoice>,
    /// The caller's own CSS.
    pub custom_css: MyCustomCss,
    /// The site's CSS and whether the caller's own applies.
    pub site: SiteAppearance,
    /// False while the caller has saved nothing to their account.
    pub stored: bool,
}

/// A person's own CSS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MyCustomCss {
    /// The sheet, empty when there is none.
    pub text: String,
    /// The "Use my CSS" switch. Off keeps the text but applies none of it.
    pub enabled: bool,
    /// When the text last changed, unix milliseconds.
    pub saved_at: Option<i64>,
}

/// The site's appearance as every signed-in person reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SiteAppearance {
    /// CSS loaded under every person's own.
    pub css: String,
    /// Whether people's own CSS applies.
    pub allow_user_css: bool,
}

/// A change to the caller's appearance. A field left out keeps its value.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct AppearanceUpdate {
    /// The theme to use.
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable = false)]
    pub theme: Option<ThemeChoice>,
    /// The caller's own CSS.
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable = false)]
    pub custom_css: Option<CustomCssUpdate>,
}

/// A change to a person's own CSS. A field left out keeps its value.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct CustomCssUpdate {
    /// The new sheet; an empty one clears it.
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable = false)]
    pub text: Option<String>,
    /// The "Use my CSS" switch.
    #[serde(default, deserialize_with = "present")]
    #[schema(nullable = false)]
    pub enabled: Option<bool>,
}

/// A field that may be left out but is never null.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// The answer to the one-time move from the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AppearanceImported {
    /// The caller's appearance as stored now.
    #[serde(flatten)]
    pub appearance: MyAppearance,
    /// True when this call wrote it; false when the account already held
    /// one and nothing changed.
    pub imported: bool,
}

/// One kept save of a person's CSS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CssSave {
    /// The save's id.
    pub id: i64,
    /// The sheet as saved.
    pub css: String,
    /// When it was saved, unix milliseconds.
    pub saved_at: i64,
}

/// What an admin sees of a person's CSS: never the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CustomCssSummary {
    /// Whether the person has CSS saved.
    pub has_css: bool,
    /// The person's "Use my CSS" switch.
    pub enabled: bool,
    /// The sheet's length in characters, as the limit counts them.
    pub length: u64,
    /// When the text last changed, unix milliseconds.
    pub saved_at: Option<i64>,
}

/// What a refused sheet answers in the error's `details`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CssRefusal {
    /// Each refused part of the sheet, once, in the order the check reads
    /// them.
    pub problems: Vec<super::check::CssProblem>,
}
