variable "name" {
  type        = string
  description = "Name prefix"
}

variable "vpc_cidr" {
  type        = string
  default     = "10.20.0.0/16"
  description = "VPC CIDR"
}
