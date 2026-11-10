//! The Ruby inside an ERB template, at the byte offsets the template has it.
//!
//! A template is HTML or text with Ruby in `<% … %>` tags. Herb reads the
//! template and returns its Ruby with everything else blanked to spaces: a
//! Ruby program of the template's length, so a span Prism reports in it is
//! a span in the template. Each tag's code ends in a `;`, the way an ERB
//! compiler joins tags into one program, and comment tags are not code.

/// `template` with everything but tag code blanked, or `None` when Herb
/// cannot read it (not UTF-8, or holding a NUL byte) or does not keep its
/// length.
pub fn ruby(template: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(template).ok()?;
    let ruby = herb::extract_ruby(text).ok()?;
    (ruby.len() == template.len()).then(|| ruby.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{RUBY_ERB_HERB_VERSION, is_erb_template as is_template};

    fn extracted(template: &str) -> String {
        String::from_utf8(ruby(template.as_bytes()).expect("readable template")).unwrap()
    }

    #[test]
    fn code_keeps_its_offsets_and_everything_else_is_blank() {
        let template = "<p>caf\u{e9}</p>\n<%= render \"card\", title: @post.title %>\n<% if admin? -%>\n  <%- link_to \"Edit\", edit_path %>\n<% end %>\n";
        let ruby = extracted(template);
        assert_eq!(ruby.len(), template.len());
        for code in [
            "render \"card\", title: @post.title",
            "if admin?",
            "link_to \"Edit\", edit_path",
            "end",
        ] {
            assert_eq!(ruby.find(code), template.find(code), "{code}");
        }
        assert_eq!(ruby.matches('\n').count(), template.matches('\n').count());
        assert_eq!(
            ruby.replace(';', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            "render \"card\", title: @post.title if admin? link_to \"Edit\", edit_path end"
        );
    }

    #[test]
    fn comments_are_not_code_and_output_tags_are() {
        assert_eq!(extracted("<%# render \"x\" %>").trim(), "");
        assert_eq!(extracted("<%== raw %>").replace(';', "").trim(), "raw");
        assert_eq!(extracted("<p>plain</p>").trim(), "");
    }

    #[test]
    fn unreadable_templates_are_refused() {
        assert_eq!(ruby(b"<%= a %>\0"), None);
        assert_eq!(ruby(&[b'<', b'%', 0xff, b'%', b'>']), None);
    }

    #[test]
    fn the_linked_herb_is_the_one_snapshots_name() {
        assert_eq!(
            format!("herb {}", herb::herb_version()),
            RUBY_ERB_HERB_VERSION
        );
    }

    #[test]
    fn templates_are_named_by_their_last_extension() {
        for path in ["app/views/posts/show.html.erb", "a.ERB", "mail.text.erb"] {
            assert!(is_template(path), "{path}");
        }
        for path in ["app/models/post.rb", ".erb", "erb", "show.html.erb.bak"] {
            assert!(!is_template(path), "{path}");
        }
    }
}
