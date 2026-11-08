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
  type    = number
  default = 5
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
