# TODO: Rusty Knowledge Implementation Roadmap

* Property-Based Tests for QueryableCache / Fake
  * I would say that QueryableCache is responsible for making sure offline-mode works as if you were online, so it needs to:
    1. Store all operations so they can be executed once one is online.
    2. Run the operations against a fake and the real system in parallel
    3. Take the result of the fake if the real system takes longer to respond
    4. Throw away the result of the fake once the real system responds
    5. Maintain a mapping
  * Take one fake as the Fake
  * Wrap an actual SourceSystem a mock
    * mock so we can easily simulate what happens if the source system
      * denies a change
      * is not available for a longer period
      * returns something conflicting
    * https://crates.io/crates/mry looks good as mock library
  * Test that the result of using Fake is equivalent to using SourceSystem after sync
  * Also allow wrapping another Fake in a mock as the SourceSystem
    * Allows running tests in case of rate limits
    * Does not test that the fake is implemented correctly, but that fake+cache behave the same way as fake alone
* Implement OperationDispatcher
