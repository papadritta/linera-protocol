// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

/*!
# Matching Engine Example Application

This sample application demonstrates a matching engine, showcasing the DeFi capabilities
on the Linera protocol.

The matching engine trades between two tokens `token_0` & `token_1`. We can refer to
the `fungible` application example on how to create two token applications.

An order can be of two types:

- Bid: For buying token 1 and paying in token 0, these are ordered in from the highest
  bid (most preferable) to the lowest price.
- Ask: For selling token 1, to be paid in token 0, these are ordered from the lowest
  (most preferable) to the highest price.

An `OrderId` is used to uniquely identify an order and enables the following functionality:

- Modify: Allows to modify the order.
- Cancel: Cancelling an order.

When inserting an order it goes through the following steps:

- Transfer of tokens from the `fungible` application to the `matching engine` application through a cross-application
call so that it can be paid to the counterparty.

- The engine selects the matching price levels for the inserted order. It then proceeds
  to clear these levels, executing trades and ensuring that at the end of the process,
  the best bid has a higher price than the best ask. This involves adjusting the orders in the market
  and potentially creating a series of transfer orders for the required tokens. If, after
  the level clearing, the order is completely filled, it is not inserted. Otherwise,
  it becomes a liquidity order in the matching engine, providing depth to the market
  and potentially being matched with future orders.

When an order is created from a remote chain, it transfers the tokens of the same owner
from the remote chain to the chain of the matching engine, and a `ExecuteOrder` message is sent with the order details.

# Usage

## Setting Up

Before getting started, make sure that the binary tools `linera*` corresponding to
your version of `linera-sdk` are in your PATH. For scripting purposes, we also assume
that the BASH function `linera_spawn_and_read_wallet_variables` is defined.

From the root of Linera repository, this can be achieved as follows:

```bash
export PATH="$PWD/target/debug:$PATH"
source /dev/stdin <<<"$(linera net helper 2>/dev/null)"
```

To start the local Linera network:

```bash
linera_spawn_and_read_wallet_variables linera net up --testing-prng-seed 37
```

We use the test-only CLI option `--testing-prng-seed` to make keys deterministic and simplify our
explanation.

```bash
OWNER_1=7136460f0c87ae46f966f898d494c4b40c4ae8c527f4d1c0b1fa0f7cff91d20f
OWNER_2=90d81e6e76ac75497a10a40e689de7b912db61a91b3ae28ed4d908e52e44ef7f
CHAIN_1=e476187f6ddfeb9d588c7b45d3df334d5501d6499b3f9ad5595cae86cce16a65
CHAIN_2=e54bdb17d41d5dbe16418f96b70e44546ccd63e6f3733ae3c192043548998ff3
```

Publish and create two `fungible` application whose `application_id` will be used as a
parameter while creating the `matching engine` example. The flag `--wait-for-outgoing-messages` waits until a quorum of validators has confirmed that all sent cross-chain messages have been delivered.

```bash
(cd examples/fungible && cargo build --release)

FUN1_APP_ID=$(linera --wait-for-outgoing-messages \
  publish-and-create examples/target/wasm32-unknown-unknown/release/fungible_{contract,service}.wasm \
    --json-argument "{ \"accounts\": {
        \"User:$OWNER_1\": \"100.\",
        \"User:$OWNER_2\": \"150.\"
    } }" \
    --json-parameters "{ \"ticker_symbol\": \"FUN1\" }" \
)

FUN2_APP_ID=$(linera --wait-for-outgoing-messages \
  publish-and-create examples/target/wasm32-unknown-unknown/release/fungible_{contract,service}.wasm \
    --json-argument "{ \"accounts\": {
        \"User:$OWNER_1\": \"100.\",
        \"User:$OWNER_2\": \"150.\"
    } }" \
    --json-parameters "{ \"ticker_symbol\": \"FUN2\" }" \
)

```

Now we have to publish and deploy the Matching Engine application:

```bash
(cd examples/matching-engine && cargo build --release)
MATCHING_ENGINE=$(linera --wait-for-outgoing-messages \
    publish-and-create examples/target/wasm32-unknown-unknown/release/matching_engine_{contract,service}.wasm \
    --json-parameters "{\"tokens\":["\"$FUN1_APP_ID\"","\"$FUN2_APP_ID\""]}" \
    --required-application-ids $FUN1_APP_ID $FUN2_APP_ID)
```

## Using the Matching Engine Application

First, a node service for the current wallet has to be started:

```bash
PORT=8080
linera service --port $PORT &
```

### Using GraphiQL

Navigate to `http://localhost:8080/chains/$CHAIN_1/applications/$MATCHING_ENGINE`.

To create a `Bid` order nature:

```gql,uri=http://localhost:8080/chains/$CHAIN_1/applications/$MATCHING_ENGINE
mutation ExecuteOrder {
  executeOrder(
    order:{
        Insert : {
        owner: "User:7136460f0c87ae46f966f898d494c4b40c4ae8c527f4d1c0b1fa0f7cff91d20f",
        amount: "1",
        nature: Bid,
        price: {
            price:5
        }
      }
    }
  )
}
```

To query about the bid price:

```gql,uri=http://localhost:8080/chains/$CHAIN_1/applications/$MATCHING_ENGINE
query{
  bids {
    keys{
      price
    }
  }
}
```
*/

use async_graphql::{scalar, InputObject, Request, Response, SimpleObject};
use fungible::FungibleTokenAbi;
use linera_sdk::{
    base::{AccountOwner, Amount, ApplicationId, ContractAbi, ServiceAbi},
    graphql::GraphQLMutationRoot,
    views::{CustomSerialize, ViewError},
};
use serde::{Deserialize, Serialize};

pub struct MatchingEngineAbi;

impl ContractAbi for MatchingEngineAbi {
    type InitializationArgument = ();
    type Parameters = Parameters;
    type Operation = Operation;
    type ApplicationCall = ApplicationCall;
    type Message = Message;
    type SessionCall = ();
    type Response = ();
    type SessionState = ();
}

impl ServiceAbi for MatchingEngineAbi {
    type Parameters = Parameters;
    type Query = Request;
    type QueryResponse = Response;
}

/// The asking or bidding price of token 1 in units of token 0.
///
/// Forgetting about types and units, if `account` is buying `quantity` for a `price`:
/// ```ignore
/// account[0] -= price * quantity;
/// account[1] += quantity;
/// ```
/// Thus the quantity (also called _count_) is an `Amount`.
///
/// When we have ask > bid then the winner for the residual cash is the liquidity provider.
/// We choose to force the price to be an integer u64. This is because the tokens are undivisible.
/// In practice, this means that the value of token1 has to be much higher than the price of token0
/// just as in a normal market where the price is in multiple of cents.
///
/// TODO(#841): Implementing fractional price is preferable for some exchanges. This cause some
/// technical issues with the serialization because we want the serialization order to be the
/// same as the original fractions. One way is to keep the serialization order we can limit
/// ourselves to fractions of the form say x / 100000.
/// The next problem is that if we do the fractions, then the order can only be filled partially. And
/// in a mathematical way, Euclidean divisions have to be done.
#[derive(
    Clone, Copy, Debug, PartialEq, PartialOrd, Deserialize, Serialize, SimpleObject, InputObject,
)]
#[graphql(input_name = "PriceInput")]
pub struct Price {
    pub price: u64,
}

impl Price {
    pub fn to_bid(&self) -> PriceBid {
        PriceBid { price: self.price }
    }
    pub fn to_ask(&self) -> PriceAsk {
        PriceAsk { price: self.price }
    }
}

#[derive(Clone, Copy, Debug, SimpleObject, InputObject)]
#[graphql(input_name = "PriceAskInput")]
pub struct PriceAsk {
    pub price: u64,
}

impl PriceAsk {
    pub fn to_price(&self) -> Price {
        Price { price: self.price }
    }
}

/// We use the custom serialization for the PriceAsk so that the order of the serialization
/// corresponds to the order of the Prices.
impl CustomSerialize for PriceAsk {
    fn to_custom_bytes(&self) -> Result<Vec<u8>, ViewError> {
        let mut short_key = bcs::to_bytes(&self.price)?;
        short_key.reverse();
        Ok(short_key)
    }

    fn from_custom_bytes(short_key: &[u8]) -> Result<Self, ViewError> {
        let mut bytes = short_key.to_vec();
        bytes.reverse();
        let price = bcs::from_bytes(&bytes)?;
        Ok(PriceAsk { price })
    }
}

#[derive(Clone, Copy, Debug, SimpleObject, InputObject)]
#[graphql(input_name = "PriceBidInput")]
pub struct PriceBid {
    pub price: u64,
}

impl PriceBid {
    pub fn to_price(&self) -> Price {
        Price { price: self.price }
    }
}

/// We use the custom serialization for the PriceAsk so that the order of the serialization
/// corresponds to the order of the Prices.
impl CustomSerialize for PriceBid {
    fn to_custom_bytes(&self) -> Result<Vec<u8>, ViewError> {
        let price_rev = u64::MAX - self.price;
        let mut short_key = bcs::to_bytes(&price_rev)?;
        short_key.reverse();
        Ok(short_key)
    }

    fn from_custom_bytes(short_key: &[u8]) -> Result<Self, ViewError> {
        let mut bytes = short_key.to_vec();
        bytes.reverse();
        let price_rev = bcs::from_bytes::<u64>(&bytes)?;
        let price = u64::MAX - price_rev;
        Ok(PriceBid { price })
    }
}

pub fn product_price_amount(price: Price, count: Amount) -> Amount {
    count.try_mul(price.price as u128).expect("product")
}

/// An identifier for a buy or sell order
pub type OrderId = u64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OrderNature {
    /// A bid for buying token 1 and paying in token 0
    Bid,
    /// An ask for selling token 1, to be paid in token 0
    Ask,
}

scalar!(OrderNature);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Order {
    /// Insertion of an order
    Insert {
        owner: AccountOwner,
        amount: Amount,
        nature: OrderNature,
        price: Price,
    },
    /// Cancelling of an order
    Cancel {
        owner: AccountOwner,
        order_id: OrderId,
    },
    /// Modifying order (only decreasing is allowed)
    Modify {
        owner: AccountOwner,
        order_id: OrderId,
        cancel_amount: Amount,
    },
}

scalar!(Order);

/// When the matching engine is created we need to create to
/// trade between two tokens 0 and 1. Those two tokens
/// are put as parameters in the creation of the matching engine
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Parameters {
    /// The token0 and token1 used for the matching engine
    pub tokens: [ApplicationId<FungibleTokenAbi>; 2],
}

scalar!(Parameters);

/// Operations that can be sent to the application.
#[derive(Debug, Deserialize, Serialize, GraphQLMutationRoot)]
pub enum Operation {
    /// The order that is going to be executed on the chain of the order book.
    ExecuteOrder { order: Order },
}

/// Messages that can be processed by the application.
#[derive(Debug, Deserialize, Serialize)]
pub enum Message {
    /// The order being transmitted from the chain and received by the chain of the order book.
    ExecuteOrder { order: Order },
}

/// Arguments for an application call to the matching engine by another application.
#[derive(Debug, Deserialize, Serialize)]
pub enum ApplicationCall {
    /// The order from the application
    ExecuteOrder { order: Order },
}

#[cfg(test)]
mod tests {
    use super::{PriceAsk, PriceBid};
    use linera_sdk::views::CustomSerialize;
    use webassembly_test::webassembly_test;

    #[webassembly_test]
    fn test_ordering_serialization() {
        let n = 20;
        let mut vec = Vec::new();
        let mut val = 1;
        for _ in 0..n {
            val *= 3;
            vec.push(val);
        }
        for i in 1..vec.len() {
            let val1 = vec[i - 1];
            let val2 = vec[i];
            assert!(val1 < val2);
            let price_ask1 = PriceAsk { price: val1 };
            let price_ask2 = PriceAsk { price: val2 };
            let price_bid1 = PriceBid { price: val1 };
            let price_bid2 = PriceBid { price: val2 };
            let ser_ask1 = price_ask1.to_custom_bytes().unwrap();
            let ser_ask2 = price_ask2.to_custom_bytes().unwrap();
            let ser_bid1 = price_bid1.to_custom_bytes().unwrap();
            let ser_bid2 = price_bid2.to_custom_bytes().unwrap();
            assert!(ser_ask1 < ser_ask2);
            assert!(ser_bid1 > ser_bid2);

            let price_ask1_back = PriceAsk::from_custom_bytes(&ser_ask1).unwrap();
            let price_ask2_back = PriceAsk::from_custom_bytes(&ser_ask2).unwrap();
            let price_bid1_back = PriceBid::from_custom_bytes(&ser_bid1).unwrap();
            let price_bid2_back = PriceBid::from_custom_bytes(&ser_bid2).unwrap();
            assert_eq!(price_ask1.price, price_ask1_back.price);
            assert_eq!(price_ask2.price, price_ask2_back.price);
            assert_eq!(price_bid1.price, price_bid1_back.price);
            assert_eq!(price_bid2.price, price_bid2_back.price);
        }
    }
}
