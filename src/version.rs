pub const GITHUB_REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Orbit Tape is derived from Flowsurface (GPL-3.0-or-later).
pub const UPSTREAM_NAME: &str = "Flowsurface";
pub const UPSTREAM_URL: &str = "https://github.com/flowsurface-rs/flowsurface";

pub const BUILD_GIT_SHA: Option<&str> = option_env!("ORBIT_TAPE_GIT_SHA");
pub const BUILD_IS_RELEASE_TAG_RAW: Option<&str> = option_env!("ORBIT_TAPE_IS_RELEASE_TAG");
pub const BUILD_IS_OFFICIAL_RELEASE_RAW: Option<&str> =
    option_env!("ORBIT_TAPE_IS_OFFICIAL_RELEASE");

pub fn app_build_version_parts() -> (String, Option<String>) {
    let is_release_tag = matches!(BUILD_IS_RELEASE_TAG_RAW, Some("true"));
    let is_official_release = matches!(BUILD_IS_OFFICIAL_RELEASE_RAW, Some("true"));

    let stable = format!("v{APP_VERSION}");
    let dev = format!("v{APP_VERSION}-dev");
    let is_stable_build = is_release_tag && is_official_release;

    let Some(sha) = BUILD_GIT_SHA else {
        return if is_stable_build {
            (stable, None)
        } else {
            (dev, None)
        };
    };

    if is_stable_build {
        return (stable, None);
    }

    (dev, Some(sha.to_string()))
}

pub fn build_commit_url() -> Option<String> {
    BUILD_GIT_SHA.map(|sha| format!("{GITHUB_REPOSITORY_URL}/commit/{sha}"))
}
