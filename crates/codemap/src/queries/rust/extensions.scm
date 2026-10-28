; ─────────────────────────────────────────────────────────────────────────
; codemap extensions to the upstream tree-sitter-rust tags.scm
;
; Adds captures the upstream tags.scm doesn't provide but the codemap
; needs:
;
;   @use.target  - the path argument to a `use` declaration
;
; Combined with the upstream @definition.* captures, this lets the
; adapter emit Imports edges from a Rust file's `use` lines without
; falling back to a separate line-scan pass.
; ─────────────────────────────────────────────────────────────────────────

; `use foo::bar::Baz;` → capture the full path. The argument shape varies
; (scoped_identifier, scoped_use_list, identifier, use_as_clause); we
; capture the outermost child so the mapper can read the full path text.

(use_declaration
  argument: (_) @use.target)

; `impl Trait for Type {}` → capture both sides so the resolver can emit
; an Implements edge from Type's node to Trait's node. v0 only matches
; simple `(type_identifier)` shapes — generic types like
; `impl<T> Trait for Foo<T>` wrap the type field in `generic_type` and
; need a follow-up extension. The codified-rule-340 deferral applies
; here too: emit an edge ONLY when the resolver returns Resolution::Unique
; for both sides (or Trait Unknown + Type Unique → ExternalSymbol fallback
; per the implements_external_fallback policy).
(impl_item
  trait: (type_identifier) @implements.trait
  type: (type_identifier) @implements.type)
