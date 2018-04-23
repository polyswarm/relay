const ERC20Relay = artifacts.require('ERC20Relay');

module.exports = function(deployer, network, accounts) {
  const NECTAR_ADDRESS = '0x9e46a38f5daabe8683e10793b06749eef7d733d1';
  const ZERO_ADDRESS = '0x0000000000000000000000000000000000000000';
  deployer.deploy(ERC20Relay, NECTAR_ADDRESS, [ZERO_ADDRESS, ZERO_ADDRESS, ZERO_ADDRESS]);
};
