; An entry is a "function"; a block or list is a "class".
(entry value: (_) @function.inside) @function.around
(value_bind value: (_) @function.inside) @function.around

(block "{" (_)* @class.inside "}") @class.around
(variant_block "{" (_)* @class.inside "}") @class.around
(list_expr "[" (_)* @class.inside "]") @class.around

(line_comment)+ @comment.around
(block_comment) @comment.around
