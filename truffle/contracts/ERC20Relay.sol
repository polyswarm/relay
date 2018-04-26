pragma solidity ^0.4.21;

import "zeppelin-solidity/contracts/ownership/Ownable.sol";
import "zeppelin-solidity/contracts/math/SafeMath.sol";
import "zeppelin-solidity/contracts/token/ERC20/ERC20.sol";
import "zeppelin-solidity/contracts/token/ERC20/SafeERC20.sol";

contract ERC20Relay is Ownable {
    using SafeMath for uint256;
    using SafeERC20 for ERC20;

    /* Verifiers */
    uint256 constant MINIMUM_VERIFIERS = 3;
    uint256 public requiredVerifiers;
    address[] private verifiers;
    mapping (address => uint256) private verifierAddressToIndex;

    /* Withdrawals */
    struct Withdrawal {
        address destination;
        uint256 amount;
        address[] approvals;
    }

    mapping (bytes32 => Withdrawal) public withdrawals;

    /* Sidechain anchoring */
    struct Anchor {
        bytes32 blockHash;
        address[] approvals;
    }

    Anchor[] public anchors;

    ERC20 private token;

    function ERC20Relay(address token_, address[] verifiers_) public {
        require(token_ != address(0));
        require(verifiers_.length >= MINIMUM_VERIFIERS);

        // Dummy verifier at index 0
        verifiers.push(address(0));

        for (uint256 i = 0; i < verifiers_.length; i++) {
            verifiers.push(verifiers_[i]);
            verifierAddressToIndex[verifiers_[i]] = i.add(1);
        }

        requiredVerifiers = calculateRequiredVerifiers();
        token = ERC20(token_);
    }

    /** Disable usage of the fallback function */
    function () external payable {
        revert();
    }

    // TODO: Allow existing verifiers to vote on adding/removing others
    function addVerifier(address addr) external onlyOwner {
        require(verifierAddressToIndex[addr] == 0);

        uint256 index = verifiers.push(addr);
        verifierAddressToIndex[addr] = index;

        requiredVerifiers = calculateRequiredVerifiers();
    }

    // TODO: Allow existing verifiers to vote on adding/removing others
    function removeVerifier(address addr) external onlyOwner {
        require(verifierAddressToIndex[addr] != 0);
        require(verifiers.length.sub(1) >= MINIMUM_VERIFIERS);

        uint256 index = verifierAddressToIndex[addr];
        require(verifiers[index] == addr);
        verifiers[index] = verifiers[verifiers.length.sub(1)];
        delete verifiers[verifiers.length.sub(1)];
        delete verifierAddressToIndex[addr];

        requiredVerifiers = calculateRequiredVerifiers();
    }

    function activeVerifiers() public view returns (address[]) {
        require(verifiers.length > 0);

        address[] memory ret = new address[](verifiers.length.sub(1));

        // Skip dummy verifier at index 0
        for (uint256 i = 1; i < verifiers.length; i++) {
            ret[i.sub(1)] = verifiers[i];
        }

        return ret;
    }

    function numberOfVerifiers() public view returns (uint256) {
        require(verifiers.length > 0);
        return verifiers.length.sub(1);
    }

    function calculateRequiredVerifiers() internal view returns(uint256) {
        return numberOfVerifiers().mul(2).div(3);
    }

    function isVerifier(address addr) public view returns (bool) {
        return verifierAddressToIndex[addr] != 0 && verifiers[verifierAddressToIndex[addr]] == addr;
    }

    modifier onlyVerifier() {
        require(isVerifier(msg.sender));
        _;
    }

    function processWithdrawal(bytes32 txHash, address destination, uint256 amount) external onlyVerifier {
        if (withdrawals[txHash].destination == address(0)) {
            withdrawals[txHash] = Withdrawal(destination, amount, new address[](0));
        }

        Withdrawal storage withdrawal = withdrawals[txHash];
        require(withdrawal.destination == destination);
        require(withdrawal.amount == amount);

        for (uint256 i = 0; i < withdrawal.approvals.length; i++) {
            require(withdrawal.approvals[i] != msg.sender);
        }

        withdrawal.approvals.push(msg.sender);

        if (withdrawal.approvals.length >= requiredVerifiers) {
            token.safeTransfer(destination, amount);
        }
    }

    function anchor(bytes32 blockHash) external onlyVerifier {
        if (anchors[anchors.length.sub(1)].blockHash != blockHash) {
            // TODO: Check required number of sigs on last block? What to do if
            // it doesn't validate?
            anchors.push(Anchor(blockHash, new address[](0)));
        }

        anchors[anchors.length.sub(1)].approvals.push(msg.sender);
    }
}
