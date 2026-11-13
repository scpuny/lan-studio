//! Embedded WebUI asset. In M1 this is a zero-dependency placeholder page;
//! from M2 onwards it will carry the Arco Design build output. Either way the
//! asset is compiled into the binary so the runtime needs no external files.

pub const INDEX_HTML: &str = include_str!("assets/index.html");

#[cfg(test)]
mod tests {
    #[test]
    fn asset_is_embedded_and_offline() {
        let html = super::INDEX_HTML;
        assert!(html.contains("Lan Studio"));
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
    }
}
