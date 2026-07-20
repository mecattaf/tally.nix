// tally — the pls module barrel (IMPLEMENTATION-PLAN M1.5). pls as the box governor (PS#5): tally
// owns the pool configuration, leases against both boxes' brokers, co-allocates the DS4 cross-box
// pair, and ships the ambient pls-lease-wrap default. Everything a consumer (jobs M2.2, the nix
// module M3.3, the CLI M3.1) needs is re-exported here.

export {
  DEFAULT_POOL_BUDGET_GB,
  PLS_CAPACITY,
  PoolRegistry,
  defaultPools,
  defaultWorkerPool,
  defaultControllerPool,
  renderPool,
  renderPoolConfig,
  type PoolDescriptor,
  type RenderedPoolConfig,
} from "./pools";

export {
  PlsBroker,
  BrokerError,
  type BrokerGrant,
  type BrokerQueued,
  type BrokerAcquireResult,
  type BrokerPoolStatus,
  type BrokerCoallocResult,
  type AcquireArgs,
} from "./broker";

export {
  Lease,
  LeaseManager,
  type AcquireOutcome,
} from "./lease";

export {
  Coallocator,
  DS4_POOLS,
  DS4_DEFAULT_WORKER_COST,
  DS4_DEFAULT_CONTROLLER_SPILL_COST,
  type CoallocPools,
  type CoallocHold,
  type CoallocQueued,
  type CoallocOutcome,
} from "./coalloc";

export {
  runWrap,
  parseWrapArgs,
  renderWrapScript,
  DEFAULT_WRAP_COST,
  type WrapOptions,
} from "./wrap";
