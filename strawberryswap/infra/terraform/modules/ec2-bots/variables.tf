variable "name" { type = string }
variable "public_subnet_ids" { type = list(string) }
variable "security_group_id" { type = string }

variable "instance_type" {
  type    = string
  default = "t3.micro"
  validation {
    condition     = contains(["t2.micro", "t3.micro"], var.instance_type)
    error_message = "Only Free Tier eligible types are allowed: t2.micro, t3.micro."
  }
}

variable "instance_count" {
  type    = number
  default = 1
}

variable "allow_over_free_tier" {
  type    = bool
  default = false
}

variable "db_host" { type = string }
variable "db_port" { type = number }
variable "db_name" { type = string }
variable "db_user" { type = string }
variable "db_password_parameter_name" { type = string }
variable "metadata_bucket_name" { type = string }
variable "metadata_bucket_arn" { type = string }
variable "metadata_kms_key_arn" { type = string }

variable "signer_role_arns" {
  type        = map(string)
  default     = {}
  description = "Signer name => the IAM role of its KMS key (modules/signing-keys). The host may assume exactly these; each signer runs as its own user, whose AWS profile assumes its role, and only those users (and root) reach instance metadata."
}
