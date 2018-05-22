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
        bool processed;
    }

    mapping (bytes32 => Withdrawal) public withdrawals;

    event WithdrawalProcessed(
        address indexed destination,
        uint256 amount,
        bytes32 txHash,
        bytes32 blockHash,
        uint256 blockNumber
    );

    /* Sidechain anchoring */
    struct Anchor {
        bytes32 blockHash;
        uint256 blockNumber;
        address[] approvals;
        bool processed;
    }

    Anchor[] public anchors;

    event AnchoredBlock(
        bytes32 indexed blockHash,
        uint256 indexed blockNumber
    );

    ERC20 private token;

    constructor(address token_, address[] verifiers_) public {
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
        require(addr != address(0));
        require(verifierAddressToIndex[addr] == 0);

        uint256 index = verifiers.push(addr);
        verifierAddressToIndex[addr] = index.sub(1);

        requiredVerifiers = calculateRequiredVerifiers();
    }

    // TODO: Allow existing verifiers to vote on adding/removing others
    function removeVerifier(address addr) external onlyOwner {
        require(addr != address(0));
        require(verifierAddressToIndex[addr] != 0);
        require(verifiers.length.sub(1) > MINIMUM_VERIFIERS);

        uint256 index = verifierAddressToIndex[addr];
        require(verifiers[index] == addr);
        verifiers[index] = verifiers[verifiers.length.sub(1)];
        delete verifierAddressToIndex[addr];
        verifiers.length--;

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

    function approveWithdrawal(
        address destination,
        uint256 amount,
        bytes32 txHash,
        bytes32 blockHash,
        uint256 blockNumber
    )
        external
        onlyVerifier
    {
        bytes32 hash = keccak256(txHash, blockHash, blockNumber);

        if (withdrawals[hash].destination == address(0)) {
            withdrawals[hash] = Withdrawal(destination, amount, new address[](0), false);
        }

        Withdrawal storage w = withdrawals[hash];
        require(w.destination == destination);
        require(w.amount == amount);

        for (uint256 i = 0; i < w.approvals.length; i++) {
            require(w.approvals[i] != msg.sender);
        }

        w.approvals.push(msg.sender);

        if (w.approvals.length >= requiredVerifiers && !w.processed) {
            token.safeTransfer(destination, amount);
            w.processed = true;
            emit WithdrawalProcessed(destination, amount, txHash, blockHash, blockNumber);
        }
    }

    // Allow verifiers to retract their withdrawals in the case of a chain
    // reorganization. This shouldn't happen but is possible.
    function unapproveWithdrawal(
        bytes32 txHash,
        bytes32 blockHash,
        uint256 blockNumber
    )
        external
        onlyVerifier
    {
        bytes32 hash = keccak256(txHash, blockHash, blockNumber);
        require(withdrawals[hash].destination != address(0));

        Withdrawal storage w = withdrawals[hash];
        require(!w.processed);

        uint256 length = w.approvals.length;
        for (uint256 i = 0; i < length; i++) {
            if (w.approvals[i] == msg.sender) {
                w.approvals[i] = w.approvals[length.sub(1)];
                delete w.approvals[i];
                w.approvals.length--;
                break;
            }
        }
    }

    function anchor(bytes32 blockHash, uint256 blockNumber) external onlyVerifier {
        if (anchors.length == 0 ||
            anchors[anchors.length.sub(1)].blockHash != blockHash ||
            anchors[anchors.length.sub(1)].blockNumber != blockNumber) {

            // TODO: Check required number of sigs on last block? What to do if
            // it doesn't validate?
            anchors.push(Anchor(blockHash, blockNumber, new address[](0), false));
        }

        Anchor storage a = anchors[anchors.length.sub(1)];
        require(a.blockHash == blockHash);
        require(a.blockNumber == blockNumber);

        for (uint256 i = 0; i < a.approvals.length; i++) {
            require(a.approvals[i] != msg.sender);
        }

        a.approvals.push(msg.sender);
        if (a.approvals.length >= requiredVerifiers && !a.processed) {
            a.processed = true;
            emit AnchoredBlock(blockHash, blockNumber);
        }
    }

    function unanchor() external onlyVerifier {
        Anchor storage a = anchors[anchors.length.sub(1)];
        require(!a.processed);

        uint256 length = a.approvals.length;
        for (uint256 i = 0; i < length; i++) {
            if (a.approvals[i] == msg.sender) {
                a.approvals[i] = a.approvals[length.sub(1)];
                delete a.approvals[i];
                a.approvals.length--;
                break;
            }
        }
    }
}
