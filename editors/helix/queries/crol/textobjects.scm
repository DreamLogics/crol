; `e` selects an entry, `t` a block or list, `a` an element of a list or a call,
; and `c` a comment.
(entry value: (_) @entry.inside) @entry.around
(value_bind value: (_) @entry.inside) @entry.around

(block "{" (_)* @class.inside "}") @class.around
(variant_block "{" (_)* @class.inside "}") @class.around
(list_expr "[" ((_) @class.inside ","?)* "]") @class.around

(list_expr ((_) @parameter.inside . ","? @parameter.around) @parameter.around)
(tuple_expr ((_) @parameter.inside . ","? @parameter.around) @parameter.around)
(arg_list ((_) @parameter.inside . ","? @parameter.around) @parameter.around)

(line_comment)+ @comment.around
(block_comment) @comment.around
