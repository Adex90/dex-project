variable "region" {
  type    = string
  default = "us-east-1"
}

variable "name" {
  type    = string
  default = "strawberryswap"
}

variable "alert_email" {
  type        = string
  description = "Receives AWS Budget alerts (Free Tier guard)"
}

variable "monthly_budget_usd" {
  type        = number
  default     = 20
  description = "Alert threshold. The testnet's signing keys cost about $1 a month each (5 with a deployer key) plus ~$0.15 per 10,000 signatures: the relayer's and the notary's ~15 s cadence is about 173,000 each a month (~$2.60 each)."
}

variable "deployer_principal_arns" {
  type        = list(string)
  default     = []
  description = "IAM roles or users that may sign with the deployer's KMS key (forge script --aws). Empty: no deployer key; deploy with a hardware wallet instead."
}

variable "instance_type" {
  type    = string
  default = "t3.micro"
}

variable "instance_count" {
  type    = number
  default = 1
}

variable "allow_over_free_tier" {
  type    = bool
  default = false
}

variable "db_instance_class" {
  type    = string
  default = "db.t3.micro"
}

variable "db_deletion_protection" {
  type    = bool
  default = true
}
