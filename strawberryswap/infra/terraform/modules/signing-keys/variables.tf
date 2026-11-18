variable "name" { type = string }

variable "signers" {
  description = <<-EOT
    One secp256k1 KMS key and one IAM role per signer, keyed by the signer's name, with the IAM principals (role
    ARNs) allowed to assume that role. The testnet's signers: the vaults' operator, relayer and automator (their
    host's instance role assumes them, each vault through its own AWS profile) and the notary; optionally a deployer
    (the people or CI role that run script/DeployTestnet.s.sol with `forge script --aws`). A signer moved to its own
    host is a change of its principals here.
  EOT
  type        = map(list(string))

  validation {
    condition     = alltrue([for name in keys(var.signers) : can(regex("^[a-z][a-z0-9-]{1,23}$", name))])
    error_message = "Signer names are 2 to 24 lowercase letters, digits or hyphens, starting with a letter."
  }
  validation {
    condition     = alltrue([for principals in values(var.signers) : length(principals) > 0])
    error_message = "Every signer needs at least one principal allowed to assume its role (a role nobody can assume holds a key nobody can use)."
  }
  validation {
    condition = alltrue(flatten([
      for principals in values(var.signers) : [for arn in principals : can(regex("^arn:aws[a-z-]*:iam::[0-9]{12}:(role|user)/", arn))]
    ]))
    error_message = "Principals are IAM role or user ARNs (never an account root, which would let every administrator sign)."
  }
}

variable "deletion_window_in_days" {
  type        = number
  default     = 30
  description = "Waiting period before a scheduled key deletion takes effect (7 to 30). A deleted key's address can never sign again: whatever it owns or administers on chain is stranded."
  validation {
    condition     = var.deletion_window_in_days >= 7 && var.deletion_window_in_days <= 30
    error_message = "KMS accepts a deletion window of 7 to 30 days."
  }
}
