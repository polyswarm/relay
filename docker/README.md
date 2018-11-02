This set of Dockerfiles and docker-compose configs set up two ethereum networks and
token relays between them.

In ascii art this looks like the following:

```

 +-------------+             +-------+           +-------------+
 |homechain rpc+-------------+relay 0+-----------+sidechain rpc|
 +-------------+       |     +-------+     |     +-------------+
        |              |                   |        |
        |              |                   |        |
        |              |                   |        |
 +----------------+    |     +-------+     |        |   +------------------+
 |homechain sealer|    +-----+relay 1+-----+        +---+sidechain sealer 0|
 +----------------+    |     +-------+     |        |   +------------------+
                       |                   |        |
                       |                   |        |
                       |                   |        |
                       |     --------+     |        |   +------------------+
                       +----- relay 2+-----+       +----+sidechain sealer 1|
                             --------+              |   +------------------+
                                                    |
                                                    |
                                                    |
                                                    |   +------------------+
                                                    +---+sidechain sealer 2|
                                                        +------------------+
```

Overview of Ethereum accounts:

```
- 0x31c99a06cabed34f97a78742225f4594d1d16677 sidechain sealer 0
- 0x6aae54b496479a25cacb63aa9dc1e578412ee68c sidechain sealer 1
- 0x850a2f35553f8a79da068323cbc7c9e1842585d5 sidechain sealer 2
- 0xb8a26662fc7fa93e8d525f6e9d8c90fcdb467aa1 sidechain eth holder
- 0x58b6cb03655999e2ff76072d8836051ac5ddcad7 homechain sealer
- 0x32fe67b633d8880f6356ccb688d11718f490a135 homechain eth holder
- 0x0f57baedcf2c84383492d1ea700835ce2492c48a homechain fee wallet
- 0xe6cc4b147e3b1b59d2ac2f2f3784bbac1774bbf7 relay 0
- 0x28fad0751f8f406d962d27b60a2a47ccceeb8096 relay 1
- 0x87cb0b17cf9ebcb0447da7da55c703812813524b relay 2
```

To run:

1. `docker-compose -f compose/sidechain.yml -f compose/homechain.yml -f compose/relay.yml -f compose/networks.yml -f compose/config.yml up --build`
1. From polyswarm/contracts Interact with contracts through `truffle console --network={homechain|sidechain]`
