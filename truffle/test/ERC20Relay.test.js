import ether from './helpers/ether';
import advanceToBlock, { advanceBlock } from './helpers/advanceToBlock';
import EVMRevert from './helpers/EVMRevert';
import utils from 'ethereumjs-util';

const BigNumber = web3.BigNumber;

require('chai')
  .use(require('chai-as-promised'))
  .use(require('chai-bignumber')(BigNumber))
  .should();

const NectarToken = artifacts.require('NectarToken');
const ERC20Relay = artifacts.require('ERC20Relay');

contract('ERC20Relay', function ([owner, verifier0, verifier1, verifier2]) {
  beforeEach(async function () {
    this.token = await NectarToken.new();
    this.relay = await ERC20Relay.new(this.token.address, [verifier0, verifier1, verifier2]);
  });

  describe('constructor', function() {
    it('should require a valid token address', async function () {
      await ERC20Relay.new('0x0000000000000000000000000000000000000000', [verifier0, verifier1, verifier2], { from: owner }).should.be.rejectedWith(EVMRevert);
    });

    it('should require at least MINIMUM_VERIFIERS', async function () {
      await ERC20Relay.new(this.token.address, [verifier0], { from: owner }).should.be.rejectedWith(EVMRevert);
    });

    it('should have two required verifiers', async function () {
      const required_verifiers = await this.relay.requiredVerifiers();
      required_verifiers.should.be.bignumber.equal(2);
    });
  });

});
