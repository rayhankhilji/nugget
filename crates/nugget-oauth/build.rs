//! Nugget's own OAuth client ids are baked in at build time from the environment (release CI
//! sets them from GitHub variables); see `docs/oauth-apps.md`. Nothing is committed.

fn main() {
    for var in [
        "NUGGET_OAUTH_GOOGLE_CLIENT_ID",
        "NUGGET_OAUTH_GOOGLE_CLIENT_SECRET",
        "NUGGET_OAUTH_MICROSOFT_CLIENT_ID",
        "NUGGET_OAUTH_GITHUB_CLIENT_ID",
        "NUGGET_OAUTH_LINEAR_CLIENT_ID",
        "NUGGET_OAUTH_SLACK_CLIENT_ID",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
