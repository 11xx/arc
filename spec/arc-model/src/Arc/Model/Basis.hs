{-# LANGUAGE RecordWildCards #-}

-- | What a decision rests on, and why it refused.
--
-- A permission is not a boolean. It is a basis naming the exact facts the
-- model relied upon, so a reader can check every one of them against the
-- ledger. A refusal is a structured reason, not a sentence.
module Arc.Model.Basis
  ( Decision (..)
  , DecisionBasis (..)
  , Authorization (..)
  , Refusal (..)
  , MovedFact (..)
  , refusalTag
  , refusalText
  , basisText
  , isPermitted
  ) where

import Data.Set (Set)

import Arc.Model.History (Authorization (..), Closure, VerdictKind)
import Arc.Model.Identifiers
import Arc.Model.Observation

-- | What an integration would rest on if it ran now.
data DecisionBasis = DecisionBasis
  { basisPatchset :: PatchsetId
  , basisHead :: Revision
  , basisTree :: TreeId
  , basisTargetBranch :: TargetBranch
  , basisTarget :: Revision
  , basisPolicy :: Policy
  , basisAuthorization :: Authorization
  , basisGates :: [(GateName, EventId, DeclarationId)]
  -- ^ One covered, passing evaluation per required gate.
  , basisConsumedFindings :: [FindingId]
  -- ^ The blocking-finding vector that had to be empty.
  , basisConsumedHolds :: [HoldId]
  -- ^ The hold vector that had to be empty.
  }
  deriving (Eq, Ord, Show)

data Decision
  = Permitted DecisionBasis
  | Refused Refusal
  deriving (Eq, Ord, Show)

isPermitted :: Decision -> Bool
isPermitted (Permitted _) = True
isPermitted (Refused _) = False

-- | An observation that moved between the decision and the requested
-- execution. The recorded basis is not reusable.
data MovedFact
  = MovedHead Revision Revision
  | MovedTarget Revision Revision
  | MovedTree TreeId TreeId
  | MovedPolicy Policy Policy
  | MovedPatchset PatchsetId PatchsetId
  deriving (Eq, Ord, Show)

data Refusal
  = RefusedClosed Closure
  | RefusedIterating
  | RefusedNoPatchset
  | RefusedBlockedBy [ChangeId]
  | RefusedHeadMoved Revision Revision
  | RefusedBlockingFindings [FindingId]
  | RefusedContestedVerdict [EventId]
  | RefusedVerdictStands VerdictKind EventId
  | RefusedStaleApproval EventId PatchsetId
  | RefusedSelfApproval EventId ActorId (Set ActorId)
  | RefusedNoApproval
  | RefusedGates [GateRefusal]
  | RefusedHoldActive HoldId
  | RefusedUndeclaredActor
  | RefusedBasisMoved [MovedFact]
  deriving (Eq, Ord, Show)

-- | A stable short name for a refusal, for test output and mutant
-- comparison.
refusalTag :: Refusal -> String
refusalTag refusal = case refusal of
  RefusedClosed _ -> "closed"
  RefusedIterating -> "iterating"
  RefusedNoPatchset -> "no-patchset"
  RefusedBlockedBy _ -> "blocked-by"
  RefusedHeadMoved _ _ -> "head-moved"
  RefusedBlockingFindings _ -> "blocking-findings"
  RefusedContestedVerdict _ -> "contested-verdict"
  RefusedVerdictStands _ _ -> "verdict-stands"
  RefusedStaleApproval _ _ -> "stale-approval"
  RefusedSelfApproval _ _ _ -> "self-approval"
  RefusedNoApproval -> "no-approval"
  RefusedGates _ -> "gates"
  RefusedHoldActive _ -> "hold-active"
  RefusedUndeclaredActor -> "undeclared-actor"
  RefusedBasisMoved _ -> "basis-moved"

refusalText :: Refusal -> String
refusalText refusal = case refusal of
  RefusedClosed _ -> "the change is closed"
  RefusedIterating -> "the change declares it is iterating"
  RefusedNoPatchset -> "no patchset is recorded"
  RefusedBlockedBy changes -> "blocked by " <> unwords [show c | c <- changes]
  RefusedHeadMoved observed recorded ->
    "head " <> show observed <> " is not the recorded patchset head " <> show recorded
  RefusedBlockingFindings findings -> "blocking findings are open: " <> unwords [show f | f <- findings]
  RefusedContestedVerdict events ->
    "the verdict chain is contested: " <> unwords [show e | e <- events]
  RefusedVerdictStands kind event ->
    "a " <> show kind <> " verdict at " <> show event <> " stands on the current patchset"
  RefusedStaleApproval event patchset ->
    "the approval at " <> show event <> " binds to " <> show patchset <> ", not the latest patchset"
  RefusedSelfApproval event actor contributors ->
    "approval at " <> show event <> " from " <> show actor
      <> " is not independent of contributors " <> show contributors
  RefusedNoApproval -> "no approval and no waiver is recorded"
  RefusedGates refusals -> unwords (map gateRefusalText refusals)
  RefusedHoldActive hold -> "hold " <> show hold <> " is active"
  RefusedUndeclaredActor -> "the acting identity is not declared and policy requires one"
  RefusedBasisMoved moved ->
    "the basis moved: " <> unwords (map movedText moved)
  where
    movedText moved = case moved of
      MovedHead before after -> "head " <> show before <> " -> " <> show after
      MovedTarget before after -> "target " <> show before <> " -> " <> show after
      MovedTree before after -> "tree " <> show before <> " -> " <> show after
      MovedPolicy before after -> "policy " <> show before <> " -> " <> show after
      MovedPatchset before after -> "patchset " <> show before <> " -> " <> show after

basisText :: DecisionBasis -> String
basisText DecisionBasis {..} =
  unwords
    [ "patchset=" <> show basisPatchset
    , "head=" <> show basisHead
    , "tree=" <> show basisTree
    , "target=" <> show basisTarget
    , "authorization=" <> show basisAuthorization
    ]
