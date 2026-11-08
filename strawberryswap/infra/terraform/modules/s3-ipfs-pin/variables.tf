variable "name" { type = string }

variable "force_destroy" {
  type        = bool
  default     = false
  description = "Allow `terraform destroy` to delete a non-empty bucket. Keep false in prod."
}
