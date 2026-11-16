variable "ssh_allowed_cidr" {
  type        = string
  default     = ""
  description = "Optional single IPv4 or IPv6 CIDR allowed to connect to SSH port 22/TCP (e.g. 203.0.113.10/32). Leave empty to keep SSH closed."

  validation {
    condition     = var.ssh_allowed_cidr == "" || can(cidrhost(var.ssh_allowed_cidr, 0))
    error_message = "ssh_allowed_cidr must be empty or a valid CIDR block (for example 203.0.113.10/32)."
  }
}
