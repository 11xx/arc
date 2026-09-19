-- | An independent semantic model of arc's authorization.
--
-- This package characterizes the semantics of an existing arc revision. It
-- is not a port of the Rust implementation, and it is not a production
-- dependency: nothing in the Rust build or its gates reads it.
--
-- The proposed candidate\/evaluation\/selection protocol is out of scope.
-- The single-slot waiver query this model reproduces and a multi-obligation
-- representation are different semantics, and this package does not claim
-- differential compatibility with either.
module Arc.Model
  ( module Arc.Model.Identifiers
  , module Arc.Model.Observation
  , module Arc.Model.History
  , module Arc.Model.Basis
  , module Arc.Model.Decision
  , module Arc.Model.Debt
  , module Arc.Model.Audit
  , comparisonRevision
  ) where

import Arc.Model.Audit
import Arc.Model.Basis
import Arc.Model.Debt
import Arc.Model.Decision
import Arc.Model.History
import Arc.Model.Identifiers
import Arc.Model.Observation

-- | The arc revision whose authorization semantics this model
-- characterizes. The model describes behaviour, not a commit graph: a
-- revision is pinned so a reader can compare against a fixed implementation
-- rather than a moving branch.
comparisonRevision :: String
comparisonRevision = "df47db0b559853af567362901e3027231b2f9d1d"
