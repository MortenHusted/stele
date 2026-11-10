//! The Ruby inside an ERB template, at the byte offsets the template has it.
//!
//! A template is HTML or text with Ruby in `<% … %>` tags. Blanking
//! everything else to spaces leaves a Ruby program of the same length, so a
//! span Prism reports in it is a span in the template. Each tag's code ends
//! in a `;`, written over its closing delimiter, the way an ERB compiler
//! joins tags into one program.
//!
//! This reads ERB's tag syntax only. It does not parse the surrounding
//! markup, and a `%>` inside a Ruby string ends the tag here as it does in
//! ERB itself.

/// `template` with everything but tag code blanked. Line breaks stay, so
/// lines and columns agree with the template as well as bytes do.
pub fn ruby(template: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = template
        .iter()
        .map(|&byte| {
            if matches!(byte, b'\n' | b'\r') {
                byte
            } else {
                b' '
            }
        })
        .collect();
    let mut at = 0;
    while let Some(open) = find(template, at, b"<%") {
        let mut code = open + 2;
        // `<%%` is a literal `<%` in the output, not a tag.
        if template.get(code) == Some(&b'%') {
            at = code + 1;
            continue;
        }
        let comment = template.get(code) == Some(&b'#');
        // `<%=`, `<%==` and `<%-` mark output or trimming, not code.
        while matches!(template.get(code), Some(b'=' | b'-')) {
            code += 1;
        }
        let Some(close) = closing(template, code) else {
            break;
        };
        if !comment {
            let mut end = close;
            if end > code && template[end - 1] == b'-' {
                end -= 1;
            }
            out[code..end].copy_from_slice(&template[code..end]);
            out[close] = b';';
        }
        at = close + 2;
    }
    out
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

/// The `%>` that ends a tag whose code starts at `from`; `%%>` is a literal
/// `%>` inside the tag.
fn closing(template: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    loop {
        let close = find(template, at, b"%>")?;
        if close > from && template[close - 1] == b'%' {
            at = close + 2;
        } else {
            return Some(close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::is_erb_template as is_template;

    fn extracted(template: &str) -> String {
        let out = ruby(template.as_bytes());
        assert_eq!(out.len(), template.len());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn code_keeps_its_offsets_and_everything_else_is_blank() {
        let template = "<p>caf\u{e9}</p>\n<%= render \"card\", title: @post.title %>\n<% if admin? -%>\n  <%- link_to \"Edit\", edit_path %>\n<% end %>\n";
        let ruby = extracted(template);
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
            ruby.split_whitespace().collect::<Vec<_>>().join(" "),
            "render \"card\", title: @post.title ; if admin? ; link_to \"Edit\", edit_path ; end ;"
        );
    }

    #[test]
    fn comments_literal_delimiters_and_unclosed_tags_are_not_code() {
        assert_eq!(extracted("<%# render \"x\" %>").trim(), "");
        assert_eq!(extracted("<%% not_a_tag %>").trim(), "");
        assert_eq!(extracted("a <% open").trim(), "");
        assert_eq!(extracted("<% puts \"%%>\" %>").trim(), "puts \"%%>\" ;");
        assert_eq!(extracted("<%== raw %>").trim(), "raw ;");
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
