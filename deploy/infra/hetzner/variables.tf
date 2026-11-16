variable "hcloud_token" {
  type        = string
  default     = null
  sensitive   = true
  description = "Hetzner Cloud API token. Leave null to use the HCLOUD_TOKEN environment variable."

  validation {
    condition     = var.hcloud_token == null || length(trimspace(var.hcloud_token)) > 0
    error_message = "hcloud_token must be null or a non-empty string."
  }
}

variable "location" {
  type        = string
  default     = "fsn1"
  description = "Hetzner Cloud location for the server and primary IP (for example fsn1, nbg1, hel1, ash, hil, sin)."

  validation {
    condition     = length(trimspace(var.location)) > 0 && can(regex("^[a-z0-9-]+$", var.location))
    error_message = "location must be a lowercase Hetzner Cloud location identifier (for example fsn1)."
  }
}

variable "name_prefix" {
  type        = string
  default     = "zenha"
  description = "Base name prefix for Hetzner Cloud resources."

  validation {
    condition     = length(var.name_prefix) >= 2 && length(var.name_prefix) <= 20 && can(regex("^[a-z0-9][a-z0-9-]*[a-z0-9]$", var.name_prefix))
    error_message = "name_prefix must be 2 to 20 lowercase alphanumeric characters or hyphens."
  }
}

variable "server_type" {
  type        = string
  default     = "cx23"
  description = "Hetzner Cloud server type (for example cx23, cpx11, cax11)."

  validation {
    condition     = length(trimspace(var.server_type)) > 0 && can(regex("^[a-z0-9-]+$", var.server_type))
    error_message = "server_type must be a valid Hetzner Cloud server type identifier."
  }
}

variable "image" {
  type        = string
  default     = "ubuntu-24.04"
  description = "Base OS image for the Hetzner Cloud server."

  validation {
    condition     = length(trimspace(var.image)) > 0
    error_message = "image must not be empty."
  }
}

variable "ssh_public_key" {
  type        = string
  description = "SSH public key uploaded to Hetzner Cloud and injected into the server (e.g. ssh-ed25519 AAAA...)."

  validation {
    condition     = can(regex("^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp256|sk-ssh-ed25519@openssh\\.com|sk-ecdsa-sha2-nistp256@openssh\\.com)\\s+", trimspace(var.ssh_public_key)))
    error_message = "ssh_public_key must be a valid OpenSSH public key."
  }
}

variable "ssh_allowed_cidr" {
  type        = string
  default     = ""
  description = "Optional single IPv4 or IPv6 CIDR allowed to connect to SSH port 22/TCP (e.g. 203.0.113.10/32). Leave empty to keep SSH closed."

  validation {
    condition     = var.ssh_allowed_cidr == "" || can(cidrhost(var.ssh_allowed_cidr, 0))
    error_message = "ssh_allowed_cidr must be empty or a valid CIDR block (for example 203.0.113.10/32)."
  }
}

variable "enable_ipv6" {
  type        = bool
  default     = true
  description = "Whether to enable a public IPv6 subnet alongside the static Primary IPv4."
}
