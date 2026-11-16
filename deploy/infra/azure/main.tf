module "host" {
  source = "../modules/host"

  ssh_allowed_cidr = var.ssh_allowed_cidr
}

data "azurerm_resource_group" "main" {
  name = var.resource_group_name
}

resource "azurerm_network_security_group" "main" {
  name                = "${var.name_prefix}-nsg"
  location            = var.location
  resource_group_name = data.azurerm_resource_group.main.name

  dynamic "security_rule" {
    for_each = module.host.ports
    content {
      name                       = security_rule.value.name
      priority                   = security_rule.value.priority
      direction                  = "Inbound"
      access                     = "Allow"
      protocol                   = title(security_rule.value.protocol)
      source_port_range          = "*"
      destination_port_range     = tostring(security_rule.value.port)
      source_address_prefix      = coalesce(security_rule.value.source_cidr, "*")
      destination_address_prefix = "*"
      description                = security_rule.value.description
    }
  }
}

resource "azurerm_virtual_network" "main" {
  name                = "${var.name_prefix}-vnet"
  location            = var.location
  resource_group_name = data.azurerm_resource_group.main.name
  address_space       = ["10.10.0.0/16"]
}

resource "azurerm_subnet" "default" {
  name                 = "default"
  resource_group_name  = data.azurerm_resource_group.main.name
  virtual_network_name = azurerm_virtual_network.main.name
  address_prefixes     = ["10.10.1.0/24"]
}

resource "azurerm_subnet_network_security_group_association" "default" {
  subnet_id                 = azurerm_subnet.default.id
  network_security_group_id = azurerm_network_security_group.main.id
}

resource "azurerm_public_ip" "main" {
  name                = "${var.name_prefix}-pip"
  location            = var.location
  resource_group_name = data.azurerm_resource_group.main.name
  allocation_method   = "Static"
  sku                 = "Standard"
  sku_tier            = "Regional"
  ip_version          = "IPv4"
  domain_name_label   = var.dns_label_prefix
}

resource "azurerm_network_interface" "main" {
  name                = "${var.name_prefix}-nic"
  location            = var.location
  resource_group_name = data.azurerm_resource_group.main.name

  ip_configuration {
    name                          = "ipconfig1"
    subnet_id                     = azurerm_subnet.default.id
    private_ip_address_allocation = "Dynamic"
    public_ip_address_id          = azurerm_public_ip.main.id
    primary                       = true
  }
}

resource "azurerm_network_interface_security_group_association" "main" {
  network_interface_id      = azurerm_network_interface.main.id
  network_security_group_id = azurerm_network_security_group.main.id
}

resource "azurerm_linux_virtual_machine" "main" {
  name                            = "${var.name_prefix}-vm"
  computer_name                   = "${var.name_prefix}-vm"
  resource_group_name             = data.azurerm_resource_group.main.name
  location                        = var.location
  size                            = var.vm_size
  admin_username                  = var.admin_username
  disable_password_authentication = true
  network_interface_ids           = [azurerm_network_interface.main.id]
  custom_data                     = base64encode(module.host.cloud_init)
  patch_mode                      = "ImageDefault"

  identity {
    type = "SystemAssigned"
  }

  admin_ssh_key {
    username   = var.admin_username
    public_key = trimspace(var.ssh_public_key)
  }

  os_disk {
    name                 = "${var.name_prefix}-osdisk"
    caching              = "ReadWrite"
    storage_account_type = "StandardSSD_LRS"
    disk_size_gb         = var.os_disk_size_gb
  }

  source_image_reference {
    publisher = "Canonical"
    offer     = "ubuntu-24_04-lts"
    sku       = "server"
    version   = "latest"
  }

  # Azure omits osProfile.customData on GET, so ignore drift after initial provisioning/import.
  lifecycle {
    ignore_changes = [custom_data]
  }
}
