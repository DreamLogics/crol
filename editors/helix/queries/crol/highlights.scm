; Syntax highlighting for CROL, with the scopes of Helix. Where several patterns
; match a node, the last one counts, so the general ones come first.

; Operators and punctuation.
["+" "-" "*" "/" "%" "~" "~^" "|" "??" "^" "&" "!" "?" ":=" "=" ".."] @operator
[":" ";" "," "." "::"] @punctuation.delimiter
["{" "}" "[" "]" "(" ")"] @punctuation.bracket
["use" "schema" "type" "extends" "variant" "as" "super"] @keyword

; Comments.
(line_comment) @comment.line
((line_comment) @comment.line.documentation
  (#match? @comment.line.documentation "^///"))
(block_comment) @comment.block

; Literals.
[(string) (raw_string)] @string
(char) @constant.character
(integer) @constant.numeric.integer
(float) @constant.numeric.float
["true" "false"] @constant.builtin.boolean
(id) @string.special.symbol

; Keys and fields.
(key (identifier) @variable.other.member)
(key (string) @variable.other.member)
(field_selector name: (identifier) @variable.other.member)
(index_selector index: (tuple_index) @variable.other.member)
(type_step name: (identifier) @variable.other.member)
(type_step index: (tuple_index) @variable.other.member)

; Type names, variants and constructors.
(block type: (identifier) @type)
(named_tuple name: (identifier) @constructor)
(macro_call name: (identifier) @function.macro)
(unit_name (identifier) @type.enum.variant)
(type_attr name: (identifier) @type)
(use_alias name: (identifier) @type)
(type_expr (identifier) @type)

; References and binds.
[(self) (root) (parent) (default)] @variable.builtin
(reference "&" @variable.builtin)
(bind_name) @variable
(bind name: (identifier) @variable)
(value_bind name: (identifier) @variable)

; Variants.
(cond_name (identifier) @label)
(variant ["<" ">"] @punctuation.special)

; Attributes.
(attribute ["#" "!"] @punctuation.special)
(entry_attr "#" @punctuation.special)
