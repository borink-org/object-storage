pub(crate) fn valid_http_origin(value: &str) -> bool {
    let Some((scheme, authority)) = value.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https")
        && !authority.is_empty()
        && value.is_ascii()
        && !authority.bytes().any(|byte| {
            byte.is_ascii_control() || matches!(byte, b' ' | b'/' | b'?' | b'#' | b'@' | b'\\')
        })
}
