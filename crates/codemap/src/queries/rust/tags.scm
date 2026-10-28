; ─────────────────────────────────────────────────────────────────────────
; Vendored from tree-sitter-rust 0.24.2 (queries/tags.scm)
; Source: https://github.com/tree-sitter/tree-sitter-rust
; License: MIT (see queries/rust/LICENSE-tree-sitter-rust.txt)
;
; Capture vocabulary follows the tree-sitter "code navigation" convention:
;
;   @definition.class      - struct, enum, union, type alias (ADTs)
;   @definition.method     - function inside an impl block
;   @definition.function   - free function
;   @definition.interface  - trait
;   @definition.module     - mod
;   @definition.macro      - macro_definition
;   @reference.call        - call_expression / macro_invocation
;   @reference.implementation - impl block (consumed but not currently
;                              routed to graph edges; Implements edges
;                              await target normalization, same shape as
;                              the @reference.call deferral on Calls)
;
; codemap extensions live in queries/rust/extensions.scm.
; ─────────────────────────────────────────────────────────────────────────

; ADT definitions

(struct_item
    name: (type_identifier) @name) @definition.class

(enum_item
    name: (type_identifier) @name) @definition.class

(union_item
    name: (type_identifier) @name) @definition.class

; type aliases

(type_item
    name: (type_identifier) @name) @definition.class

; method definitions

(declaration_list
    (function_item
        name: (identifier) @name) @definition.method)

; function definitions

(function_item
    name: (identifier) @name) @definition.function

; trait definitions
(trait_item
    name: (type_identifier) @name) @definition.interface

; module definitions
(mod_item
    name: (identifier) @name) @definition.module

; macro definitions

(macro_definition
    name: (identifier) @name) @definition.macro

; references

(call_expression
    function: (identifier) @name) @reference.call

(call_expression
    function: (field_expression
        field: (field_identifier) @name)) @reference.call

(macro_invocation
    macro: (identifier) @name) @reference.call

; implementations

(impl_item
    trait: (type_identifier) @name) @reference.implementation

(impl_item
    type: (type_identifier) @name
    !trait) @reference.implementation
