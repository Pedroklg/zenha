@description('Azure region for all resources.')
param location string = 'chilecentral'

@description('Base name prefix for resources.')
@minLength(2)
@maxLength(20)
param namePrefix string = 'zenha'

@description('Virtual machine size.')
param vmSize string = 'Standard_B2ats_v2'

@description('OS managed disk size in GiB (Standard SSD).')
@minValue(30)
@maxValue(64)
param osDiskSizeGB int = 30

@description('Administrator username for the Linux VM.')
@minLength(1)
@maxLength(64)
param adminUsername string = 'azureuser'

@description('SSH public key for the administrator account (e.g. ssh-ed25519 AAAA...).')
param sshPublicKey string

@description('Globally unique DNS label for the public IP (<dnsLabelPrefix>.<location>.cloudapp.azure.com).')
@minLength(3)
@maxLength(63)
param dnsLabelPrefix string

@description('Optional single IPv4 or CIDR allowed to connect to SSH port 22/TCP (e.g. 203.0.113.10/32). Leave empty to keep SSH closed.')
param sshAllowedCidr string = ''

var nsgName = '${namePrefix}-nsg'
var vnetName = '${namePrefix}-vnet'
var subnetName = 'default'
var publicIpName = '${namePrefix}-pip'
var nicName = '${namePrefix}-nic'
var vmName = '${namePrefix}-vm'
var osDiskName = '${namePrefix}-osdisk'

var baseSecurityRules = [
  {
    name: 'AllowHttpInbound'
    properties: {
      description: 'HTTP for ACME HTTP-01 challenges and HTTPS redirect (Caddy)'
      protocol: 'Tcp'
      sourcePortRange: '*'
      destinationPortRange: '80'
      sourceAddressPrefix: '*'
      destinationAddressPrefix: '*'
      access: 'Allow'
      priority: 100
      direction: 'Inbound'
    }
  }
  {
    name: 'AllowHttpsInbound'
    properties: {
      description: 'HTTPS for token server and LiveKit WSS signaling (Caddy)'
      protocol: 'Tcp'
      sourcePortRange: '*'
      destinationPortRange: '443'
      sourceAddressPrefix: '*'
      destinationAddressPrefix: '*'
      access: 'Allow'
      priority: 110
      direction: 'Inbound'
    }
  }
  {
    name: 'AllowLiveKitRtcTcpInbound'
    properties: {
      description: 'LiveKit WebRTC ICE/TCP fallback'
      protocol: 'Tcp'
      sourcePortRange: '*'
      destinationPortRange: '7881'
      sourceAddressPrefix: '*'
      destinationAddressPrefix: '*'
      access: 'Allow'
      priority: 120
      direction: 'Inbound'
    }
  }
  {
    name: 'AllowLiveKitRtcUdpInbound'
    properties: {
      description: 'LiveKit WebRTC media over multiplexed UDP'
      protocol: 'Udp'
      sourcePortRange: '*'
      destinationPortRange: '7882'
      sourceAddressPrefix: '*'
      destinationAddressPrefix: '*'
      access: 'Allow'
      priority: 130
      direction: 'Inbound'
    }
  }
]

var sshSecurityRules = !empty(sshAllowedCidr)
  ? [
      {
        name: 'AllowSshInbound'
        properties: {
          description: 'SSH access restricted to a single operator IP/CIDR'
          protocol: 'Tcp'
          sourcePortRange: '*'
          destinationPortRange: '22'
          sourceAddressPrefix: sshAllowedCidr
          destinationAddressPrefix: '*'
          access: 'Allow'
          priority: 140
          direction: 'Inbound'
        }
      }
    ]
  : []

resource nsg 'Microsoft.Network/networkSecurityGroups@2024-05-01' = {
  name: nsgName
  location: location
  properties: {
    securityRules: concat(baseSecurityRules, sshSecurityRules)
  }
}

resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' = {
  name: vnetName
  location: location
  properties: {
    addressSpace: {
      addressPrefixes: [
        '10.10.0.0/16'
      ]
    }
    subnets: [
      {
        name: subnetName
        properties: {
          addressPrefix: '10.10.1.0/24'
          networkSecurityGroup: {
            id: nsg.id
          }
        }
      }
    ]
  }
}

resource publicIp 'Microsoft.Network/publicIPAddresses@2024-05-01' = {
  name: publicIpName
  location: location
  sku: {
    name: 'Standard'
    tier: 'Regional'
  }
  properties: {
    publicIPAllocationMethod: 'Static'
    publicIPAddressVersion: 'IPv4'
    dnsSettings: {
      domainNameLabel: dnsLabelPrefix
    }
  }
}

resource nic 'Microsoft.Network/networkInterfaces@2024-05-01' = {
  name: nicName
  location: location
  properties: {
    networkSecurityGroup: {
      id: nsg.id
    }
    ipConfigurations: [
      {
        name: 'ipconfig1'
        properties: {
          primary: true
          privateIPAllocationMethod: 'Dynamic'
          subnet: {
            id: vnet.properties.subnets[0].id
          }
          publicIPAddress: {
            id: publicIp.id
          }
        }
      }
    ]
  }
}

resource vm 'Microsoft.Compute/virtualMachines@2024-07-01' = {
  name: vmName
  location: location
  identity: {
    type: 'SystemAssigned'
  }
  properties: {
    hardwareProfile: {
      vmSize: vmSize
    }
    osProfile: {
      computerName: vmName
      adminUsername: adminUsername
      customData: loadFileAsBase64('cloud-init.yaml')
      linuxConfiguration: {
        disablePasswordAuthentication: true
        ssh: {
          publicKeys: [
            {
              path: '/home/${adminUsername}/.ssh/authorized_keys'
              keyData: sshPublicKey
            }
          ]
        }
        patchSettings: {
          patchMode: 'ImageDefault'
        }
      }
    }
    storageProfile: {
      imageReference: {
        publisher: 'Canonical'
        offer: 'ubuntu-24_04-lts'
        sku: 'server'
        version: 'latest'
      }
      osDisk: {
        name: osDiskName
        createOption: 'FromImage'
        caching: 'ReadWrite'
        diskSizeGB: osDiskSizeGB
        deleteOption: 'Delete'
        managedDisk: {
          storageAccountType: 'StandardSSD_LRS'
        }
      }
    }
    networkProfile: {
      networkInterfaces: [
        {
          id: nic.id
          properties: {
            deleteOption: 'Delete'
          }
        }
      ]
    }
  }
}

output publicIpAddress string = publicIp.properties.ipAddress
output fqdn string = publicIp.properties.dnsSettings.fqdn
output vmName string = vm.name
