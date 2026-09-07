[
  (module_def)
  (class_def)
  (struct_def)
  (enum_def)
  (annotation_def)
  (lib_def)
  (method_def)
  (fun_def)
  (macro_def)
  (call)
  (if)
  (unless)
  (while)
  (until)
  (case)
  (begin)
  (block)
  (array)
  (argument_list)
  (param_list)
] @indent.begin

[
  "end"
  ")"
  "}"
  "]"
] @indent.end

[
  "end"
  ")"
  "}"
  "]"
  (else)
  (elsif)
  (when)
  (rescue)
  (ensure)
] @indent.branch

(comment) @indent.ignore
