use snafu::Snafu;
use url::Url;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("unable to use {segment:?} as a URL path segment"))]
    InvalidPathSegment { segment: String },

    #[snafu(display("unable to append path segments to {url:?}, as it cannot be a base"))]
    CannotBeABase { url: Url },
}

/// Returns `base` with each of `segments` appended as exactly one path segment.
///
/// Use this rather than [`Url::join`] or [`Url::set_path`] to create URLs from user/untrusted input.
/// Both those methods follow the specs but are easy to use insecurely because e.g. if you provide
/// an absolute URL they replace the host from `base` and a few other things (`../` treatment).
///
/// Fails on an empty, `.` or `..` segment and on a segment containing a tab or line break.
pub fn append_path_segments(base: &Url, segments: &[&str]) -> Result<Url, Error> {
    let invalid_segment = segments.iter().find(|segment| {
        matches!(**segment, "" | "." | "..") || segment.contains(['\t', '\n', '\r'])
    });
    if let Some(segment) = invalid_segment {
        return InvalidPathSegmentSnafu { segment: *segment }.fail();
    }

    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|()| CannotBeABaseSnafu { url: base.clone() }.build())?
        .pop_if_empty()
        .extend(segments);

    Ok(url)
}

#[cfg(test)]
mod tests {
    use percent_encoding::percent_decode_str;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::root("http://keycloak:8080", "http://keycloak:8080/realms/my-realm")]
    #[case::root_with_slash("http://keycloak:8080/", "http://keycloak:8080/realms/my-realm")]
    #[case::root_path(
        "http://keycloak:8080/auth",
        "http://keycloak:8080/auth/realms/my-realm"
    )]
    #[case::root_path_with_slash(
        "http://keycloak:8080/auth/",
        "http://keycloak:8080/auth/realms/my-realm"
    )]
    fn append_keeps_the_base_path(#[case] base: &str, #[case] expected: &str) {
        let base = Url::parse(base).expect("valid base URL");
        let url = append_path_segments(&base, &["realms", "my-realm"]).expect("valid segments");

        assert_eq!(url.as_str(), expected);
    }

    #[rstest]
    #[case::absolute_url("https://evil.example.com/", "https:%2F%2Fevil.example.com%2F")]
    #[case::protocol_relative("//evil.example.com", "%2F%2Fevil.example.com")]
    #[case::traversal("../../realms/master", "..%2F..%2Frealms%2Fmaster")]
    #[case::percent_encoded_dots("%2e%2e", "%252e%252e")]
    #[case::backslash("..\\admin", "..%5Cadmin")]
    #[case::query("alice?exact=false", "alice%3Fexact=false")]
    #[case::fragment("alice#", "alice%23")]
    #[case::guest_principal_name(
        "alice_example.com#EXT#@contoso.com",
        "alice_example.com%23EXT%23@contoso.com"
    )]
    fn append_encodes_a_hostile_segment(#[case] segment: &str, #[case] expected: &str) {
        let base = Url::parse("http://keycloak:8080/users").expect("valid base URL");
        let url = append_path_segments(&base, &[segment]).expect("valid segment");

        assert_eq!(
            url.as_str(),
            format!("http://keycloak:8080/users/{expected}")
        );
    }

    /// The URL is sent as a string and parsed again by the server, so every character has to stay
    /// inside its segment after a round trip, not only in the [`Url`] built here.
    #[test]
    fn append_keeps_every_character_inside_its_segment() {
        let base = Url::parse("http://keycloak:8080/users").expect("valid base URL");
        let characters = (' '..='~').chain(['\0', '\x1b', '\x7f', 'ä', '€', '🦀']);

        for character in characters {
            let segment = format!("a{character}b");
            let url = append_path_segments(&base, &[&segment]).expect("valid segment");
            let sent = Url::parse(url.as_str()).expect("the built URL parses");

            assert_eq!(sent, url, "{segment:?} changes in a round trip");
            assert_eq!(sent.host_str(), Some("keycloak"), "{segment:?}");
            assert_eq!(sent.query(), None, "{segment:?}");
            assert_eq!(sent.fragment(), None, "{segment:?}");
            let sent_segments: Vec<&str> = sent.path_segments().into_iter().flatten().collect();
            let [_, sent_segment] = sent_segments.as_slice() else {
                panic!("{segment:?} splits the path into {sent_segments:?}");
            };
            assert_eq!(
                percent_decode_str(sent_segment).decode_utf8_lossy(),
                segment,
                "{segment:?} changes on its way to the server"
            );
        }
    }

    #[rstest]
    #[case::empty("")]
    #[case::current(".")]
    #[case::parent("..")]
    #[case::tab("ad\tmin")]
    #[case::line_feed("ad\nmin")]
    #[case::carriage_return("ad\rmin")]
    fn append_rejects_a_segment_the_url_would_change(#[case] segment: &str) {
        let base = Url::parse("http://keycloak:8080/users").expect("valid base URL");
        let error = append_path_segments(&base, &["admin", segment]).expect_err("rejected");

        assert!(matches!(error, Error::InvalidPathSegment { .. }), "{error}");
    }

    #[test]
    fn append_rejects_a_base_without_a_path() {
        let base = Url::parse("mailto:admin@example.com").expect("valid base URL");
        let error = append_path_segments(&base, &["users"]).expect_err("rejected");

        assert!(matches!(error, Error::CannotBeABase { .. }), "{error}");
    }
}
