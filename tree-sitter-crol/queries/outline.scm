; Entries, with their operation and type name: `+web: Service`.
(entry
  op: _? @context
  target: (target) @name
  value: (block type: (identifier) @context)?) @item

(remove_entry "-" @context target: (target) @name) @item

(value_bind name: (identifier) @name ":=" @context) @item

(variant_block variant: (variant) @name) @item
