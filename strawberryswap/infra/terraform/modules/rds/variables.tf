variable "name" { type = string }
variable "vpc_id" { type = string }
variable "private_subnet_ids" { type = list(string) }
variable "client_security_group_id" { type = string }

variable "instance_class" {
  type    = string
  default = "db.t3.micro"
  validation {
    condition     = contains(["db.t3.micro", "db.t4g.micro"], var.instance_class)
    error_message = "Only Free Tier eligible classes are allowed: db.t3.micro, db.t4g.micro."
  }
}

variable "engine_version" {
  type    = string
  default = "16"
}

variable "deletion_protection" {
  type    = bool
  default = true
}
