require('babel-register');
require('babel-polyfill');

module.exports = {
  networks: {
    homechain: {
      host: 'localhost',
      port: 6545,
      network_id: '1337',
      from: '0x31c99a06cabed34f97a78742225f4594d1d16677',
      gasLimit: 4704588,
    },
    sidechain: {
      host: 'localhost',
      port: 7545,
      network_id: '1338',
      from: '0xb8a26662fc7fa93e8d525f6e9d8c90fcdb467aa1',
      gasLimit: 4704588,
    },
    development: {
      host: 'localhost',
      port: 8545,
      network_id: '*',
    },
  },
  solc: {
    optimizer: {
      enabled: true,
      runs: 200
    }
  }
};
