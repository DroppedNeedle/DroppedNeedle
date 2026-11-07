//! What the appearance routes do: read the caller's view, check and save
//! a change, the one-time move from the browser, and the admin's summary
//! and switch.

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::Appearance;

use super::check::{self, CssProblem, CssRules, Limit};
use super::models::{
    AppearanceImported, AppearanceUpdate, CssSave, CustomCssSummary, MyAppearance, MyCustomCss,
    SiteAppearance,
};
use super::store::{AppearanceStore, Change, Stored, Written};

/// Why a change was not saved.
#[derive(Debug)]
pub enum AppearanceError {
    /// The sheet holds something the check refuses.
    Refused(Vec<CssProblem>),
    /// The sheet is past its length limit.
    TooLong {
        /// The limit, in characters as the check counts them.
        limit: usize,
    },
    /// The site does not let people add their own CSS.
    TurnedOff,
    /// The database could not answer.
    Store(DbError),
}

impl From<DbError> for AppearanceError {
    fn from(error: DbError) -> Self {
        Self::Store(error)
    }
}

/// Refuse `css` when the check finds anything in it.
pub fn check_sheet(css: &str, limit: Limit, rules: &CssRules) -> Result<(), AppearanceError> {
    let problems = check::problems(css, limit, rules);
    if problems
        .iter()
        .any(|problem| problem.what == check::TOO_LONG)
    {
        return Err(AppearanceError::TooLong {
            limit: limit.max(rules),
        });
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppearanceError::Refused(problems))
    }
}

/// The site section as the routes read it. A section that cannot be read
/// (logged once per revision) reads as no site CSS with people's own
/// allowed: it holds back none of the work an unreadable section must.
pub fn site(section: &Live<Appearance>) -> SiteAppearance {
    match section.read() {
        Some(section) => SiteAppearance {
            css: section.site_css.clone(),
            allow_user_css: section.allow_user_css,
        },
        None => SiteAppearance {
            css: String::new(),
            allow_user_css: true,
        },
    }
}

/// The caller's view: their row, or the defaults while they have none,
/// with the site's part.
pub fn view(stored: Option<Stored>, site: SiteAppearance) -> MyAppearance {
    let present = stored.is_some();
    let stored = stored.unwrap_or_default();
    MyAppearance {
        theme: stored.theme,
        custom_css: MyCustomCss {
            text: stored.css,
            enabled: stored.enabled,
            saved_at: stored.css_saved_at,
        },
        site,
        stored: present,
    }
}

/// The caller's appearance.
pub async fn read(
    store: &AppearanceStore,
    section: &Live<Appearance>,
    user_id: &str,
) -> Result<MyAppearance, AppearanceError> {
    let stored = store.read(user_id).await?;
    Ok(view(stored, site(section)))
}

/// `text` with what JavaScript's `trim()` removes taken off both ends. Rust's
/// `trim()` removes U+0085, which JavaScript keeps, and keeps a byte order
/// mark, which JavaScript removes.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// `update` as a checked change, or why it is refused. Every part is
/// checked before anything is written, so a refused part saves nothing.
/// With the site's switch off, a change may still clear the text, switch
/// the CSS off or set the theme, but not set text or switch it on.
fn checked(
    update: AppearanceUpdate,
    site: &SiteAppearance,
    rules: &CssRules,
) -> Result<Change, AppearanceError> {
    let css = update.custom_css.unwrap_or_default();
    let text = css.text.map(|text| {
        if js_trim(&text).is_empty() {
            String::new()
        } else {
            text
        }
    });
    if !site.allow_user_css
        && (text.as_ref().is_some_and(|text| !text.is_empty()) || css.enabled == Some(true))
    {
        return Err(AppearanceError::TurnedOff);
    }
    if let Some(text) = &text {
        check_sheet(text, Limit::User, rules)?;
    }
    Ok(Change {
        theme: update.theme,
        css: text,
        enabled: css.enabled,
    })
}

/// Check and save the caller's change, all or nothing.
pub async fn save(
    store: &AppearanceStore,
    section: &Live<Appearance>,
    rules: &CssRules,
    user_id: &str,
    update: AppearanceUpdate,
) -> Result<MyAppearance, AppearanceError> {
    let site = site(section);
    let change = checked(update, &site, rules)?;
    let stored = match store.write(user_id, change, false).await? {
        Written::Saved(stored) | Written::AlreadyStored(stored) => stored,
    };
    Ok(view(Some(stored), site))
}

/// The one-time move of what a browser kept before the account held it.
/// A caller who already has a row keeps it untouched and is answered it,
/// unchecked, as nothing is written; otherwise the change is checked and
/// saved as [`save`] does.
pub async fn import(
    store: &AppearanceStore,
    section: &Live<Appearance>,
    rules: &CssRules,
    user_id: &str,
    update: AppearanceUpdate,
) -> Result<AppearanceImported, AppearanceError> {
    let site = site(section);
    if let Some(stored) = store.read(user_id).await? {
        return Ok(AppearanceImported {
            appearance: view(Some(stored), site),
            imported: false,
        });
    }
    let change = checked(update, &site, rules)?;
    let (stored, imported) = match store.write(user_id, change, true).await? {
        Written::Saved(stored) => (stored, true),
        Written::AlreadyStored(stored) => (stored, false),
    };
    Ok(AppearanceImported {
        appearance: view(Some(stored), site),
        imported,
    })
}

/// The caller's kept saves, newest first.
pub async fn saves(
    store: &AppearanceStore,
    user_id: &str,
) -> Result<Vec<CssSave>, AppearanceError> {
    Ok(store
        .saves(user_id)
        .await?
        .into_iter()
        .map(|save| CssSave {
            id: save.id,
            css: save.css,
            saved_at: save.saved_at,
        })
        .collect())
}

/// What an admin sees of a person's CSS.
pub fn summary(stored: Option<Stored>) -> CustomCssSummary {
    let stored = stored.unwrap_or_default();
    CustomCssSummary {
        has_css: !stored.css.is_empty(),
        enabled: stored.enabled,
        length: u64::try_from(stored.css.encode_utf16().count()).unwrap_or(u64::MAX),
        saved_at: stored.css_saved_at,
    }
}
