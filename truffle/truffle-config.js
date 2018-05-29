require('babel-register');
require('babel-polyfill');

module.exports = {
  networks: {
    development: {
      host: 'localhost',
      port: 8545,
      network_id: '*',
      gas: 4700000,
    },
    homechain: {
      host: 'localhost',
      port: 6545,
      network_id: '1337',
      from: '0x32fe67b633d8880f6356ccb688d11718f490a135',
      gas: 4700000,
    },
    sidechain: {
      host: 'localhost',
      port: 7545,
      network_id: '1338',
      from: '0xb8a26662fc7fa93e8d525f6e9d8c90fcdb467aa1',
      gas: 4700000,
    },
  },
  solc: {
    optimizer: {
      enabled: true,
      runs: 200
    }
  }
};
