# TFLint configuration used by CI (.github/workflows/ci.yml, job iac-and-scripts)
# when terraform/ does not ship a .tflint.hcl of its own. If terraform/.tflint.hcl
# exists, CI uses that file instead.

config {
  # The root module is flat; lint local modules too if any are added later.
  call_module_type = "local"
}

# Bundled ruleset: deprecated syntax, unused declarations, untyped variables,
# missing version constraints, and so on.
plugin "terraform" {
  enabled = true
  preset  = "recommended"
}

# AWS ruleset: invalid instance classes and types, deprecated arguments, and
# other mistakes that `terraform validate` cannot see. Downloaded by
# `tflint --init`. Bump the version deliberately, as with any other pin.
plugin "aws" {
  enabled = true
  version = "0.49.0"
  source  = "github.com/terraform-linters/tflint-ruleset-aws"
}
