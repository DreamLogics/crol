; Syntax highlighting for CROL.

(line_comment) @comment
(block_comment) @comment

[(string) (raw_string) (char)] @string
[(integer) (float)] @number
["true" "false"] @boolean
(id) @string.special.symbol

; Keys and fields.
(key (identifier) @property)
(key (string) @property)
(field_selector name: (identifier) @property)
(index_selector index: (tuple_index) @property)

; Type names, variants and constructors.
(block type: (identifier) @type)
(named_tuple name: (identifier) @constructor)
(unit_name (identifier) @constant)
(type_attr name: (identifier) @type)
(use_alias name: (identifier) @type)
(type_expr (identifier) @type)

; References and binds.
[(self) (root) (parent) (default)] @variable.special
(reference "&" @variable.special)
(bind_name) @variable
(bind name: (identifier) @variable)
(value_bind name: (identifier) @variable)

; Variants.
(cond_name (identifier) @label)
(variant ["<" ">"] @punctuation.special)

; Attributes.
(attribute ["#" "!"] @punctuation.special)
["use" "schema" "type" "extends" "as" "super"] @keyword

; Operators and punctuation.
["+" "-" "*" "/" "%" "~" "~^" "|" "^" "&" "!" "?" ":=" "=" ".."] @operator
[":" "," "." "::"] @punctuation.delimiter
["{" "}" "[" "]" "(" ")"] @punctuation.bracket
