//! Embedded WebUI asset. The console is a single self-contained HTML file
//! produced by `components/lan-mdns/webui` (`vite-plugin-singlefile`), compiled
//! into the binary so the runtime needs no external file, CDN or network.

pub const INDEX_HTML: &str = include_str!("assets/index.html");

#[cfg(test)]
mod tests {
    /// The offline guarantee is about *fetched resources*, not about every
    /// occurrence of a URL: SVG / MathML namespace URIs and documentation
    /// links inside the bundle are never requested by the browser.
    const FORBIDDEN: [&str; 8] = [
        "<script src=",
        "<link rel=\"stylesheet\"",
        "src=\"http://",
        "src=\"https://",
        "href=\"http://",
        "href=\"https://",
        "url(http://",
        "url(https://",
    ];

    #[test]
    fn asset_is_embedded_and_offline() {
        let html = super::INDEX_HTML;
        assert!(html.contains("Lan Studio"));
        // The app shell and its styles must be inline, not linked.
        assert!(html.contains("<style"), "styles must be inlined");
        assert!(html.contains("id=\"app\""), "mount point missing");
        for forbidden in FORBIDDEN {
            assert!(
                !html.contains(forbidden),
                "offline asset rule violated: {forbidden}"
            );
        }
    }
}
