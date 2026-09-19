{-# LANGUAGE RecordWildCards #-}

-- | The ledger and its replay.
--
-- 'replay' folds append-only events into a state. It decides nothing and
-- refuses nothing; it only answers what happened. Validation is a separate
-- function over the replayed state and the observations, so a reader can
-- always ask what the ledger says without asking what it permits.
module Arc.Model.History
  ( Patchset (..)
  , VerdictKind (..)
  , VerdictRelation (..)
  , Verdict (..)
  , verdictEffectiveActor
  , Finding (..)
  , Disposition (..)
  , DebtKind (..)
  , Debt (..)
  , Audit (..)
  , Claim (..)
  , Closure (..)
  , Authorization (..)
  , IntegrationRecord (..)
  , Event (..)
  , ChangeState (..)
  , emptyState
  , replay
  , latestPatchset
  , patchsetById
  , effectiveContributors
  , activeVerdicts
  , verdictContested
  , governingVerdict
  , findingResolved
  , openBlockingFindings
  , openAuditFindings
  , debtsForPatchset
  , latestIntegration
  , historicalAuthorization
  , activeIntegration
  ) where

import Data.List (sortOn)
import Data.Set (Set)
import qualified Data.Set as Set

import Arc.Model.Identifiers
import Arc.Model.Observation (Policy, Verification (..))

-- | An immutable snapshot of the branch, bound to the contributors whose
-- work it carries.
data Patchset = Patchset
  { patchsetId :: PatchsetId
  , patchsetOrdinal :: Int
  , patchsetRevision :: Revision
  , patchsetTree :: TreeId
  , patchsetAuthor :: ActorId
  , patchsetContributors :: Set ActorId
  }
  deriving (Eq, Ord, Show)

-- | A contributor set that was never declared is the author alone. The
-- synthesized set is a compatibility reading, not a declaration.
effectiveContributors :: Patchset -> Set ActorId
effectiveContributors patchset
  | Set.null (patchsetContributors patchset) = Set.singleton (patchsetAuthor patchset)
  | otherwise = patchsetContributors patchset

data VerdictKind = Approved | ChangesRequested | CommentOnly
  deriving (Eq, Ord, Show)

data VerdictRelation = Supersedes | Corroborates
  deriving (Eq, Ord, Show)

-- | A reviewer's recorded conclusion about one patchset. 'verdictAssumed'
-- records that the identity was derived rather than declared; an assumed
-- reviewer is not the second party independence needs.
data Verdict = Verdict
  { verdictEvent :: EventId
  , verdictPatchset :: PatchsetId
  , verdictKind :: VerdictKind
  , verdictActor :: ActorId
  , verdictOnBehalfOf :: Maybe ActorId
  , verdictAssumed :: Bool
  , verdictProvisional :: Maybe String
  , verdictRelation :: VerdictRelation
  , verdictSupersedes :: Maybe EventId
  }
  deriving (Eq, Ord, Show)

verdictEffectiveActor :: Verdict -> ActorId
verdictEffectiveActor Verdict {..} = maybe verdictActor id verdictOnBehalfOf

data Finding = Finding
  { findingEvent :: EventId
  , findingId :: FindingId
  , findingPatchset :: PatchsetId
  , findingActor :: ActorId
  , findingBlocking :: Bool
  , findingAudit :: Bool
  }
  deriving (Eq, Ord, Show)

data Disposition = Disposition
  { dispositionEvent :: EventId
  , dispositionFinding :: FindingId
  , dispositionResolved :: Bool
  }
  deriving (Eq, Ord, Show)

-- | What a debt declaration says was missing. The kind is the weight: a
-- label the caller may declare, otherwise derived from the ledger.
data DebtKind
  = NothingRead
  | MergeResolutionUnread
  | RepairUnread
  | ContributorOnly
  | IndependentReview
  deriving (Eq, Ord, Show)

-- | A recorded coverage obligation. A debt with 'debtPatchset' bound waives
-- exactly that patchset; one recorded after integration carries no patchset
-- and waives nothing.
data Debt = Debt
  { debtId :: DebtId
  , debtEvent :: EventId
  , debtPatchset :: Maybe PatchsetId
  , debtDeclaredKind :: Maybe DebtKind
  , debtReason :: String
  , debtActor :: ActorId
  }
  deriving (Eq, Ord, Show)

-- | A review recorded after integration, anchored to the integrated
-- revision. Audit findings are deliberately outside the shipped set.
data Audit = Audit
  { auditEvent :: EventId
  , auditRevision :: Revision
  , auditKind :: VerdictKind
  , auditActor :: ActorId
  , auditAssumed :: Bool
  , auditFindings :: [FindingId]
  }
  deriving (Eq, Ord, Show)

-- | A liveness episode. Its expiry ends the claim, never the ledger facts
-- recorded while it ran.
data Claim = Claim
  { claimId :: ClaimId
  , claimActor :: ActorId
  , claimExpired :: Bool
  }
  deriving (Eq, Ord, Show)

data Closure = ClosedAbandoned | ClosedSuperseded
  deriving (Eq, Ord, Show)

-- | What a recorded merge actually rested on, read from the ledger.
data Authorization
  = AuthorizedByVerdict EventId
  | AuthorizedByWaiver DebtId
  | AuthorizedByVerdictUnderWaiver EventId DebtId
  deriving (Eq, Ord, Show)

-- | What one integration consumed and the exact facts it relied upon. Later
-- reviews and audits are separate events; they never rewrite this record.
data IntegrationRecord = IntegrationRecord
  { integratedEvent :: EventId
  , integratedPatchset :: PatchsetId
  , integratedHead :: Revision
  , integratedTargetBranch :: TargetBranch
  , integratedTargetBefore :: Revision
  , integratedTree :: TreeId
  , integratedAuthorization :: Authorization
  , integratedGates :: [(GateName, EventId, DeclarationId)]
  , integratedConsumedFindings :: [FindingId]
  , integratedConsumedHolds :: [HoldId]
  , integratedPolicy :: Policy
  }
  deriving (Eq, Ord, Show)

-- | The append-only ledger.
data Event
  = PatchsetRecorded Patchset
  | VerdictRecorded Verdict
  | FindingRecorded Finding
  | FindingDisposed Disposition
  | VerificationRecorded Verification
  | DebtDeclared Debt
  | AuditRecorded Audit
  | ClaimStarted Claim
  | ClaimExpired ClaimId
  | HoldSet HoldId
  | HoldReleased HoldId
  | ChangeClosed Closure
  | IteratingChanged Bool
  | IntegrationRecorded IntegrationRecord
  deriving (Eq, Ord, Show)

data ChangeState = ChangeState
  { stateChange :: ChangeId
  , statePatchsets :: [Patchset]
  , stateVerdicts :: [Verdict]
  , stateFindings :: [Finding]
  , stateDispositions :: [Disposition]
  , stateVerifications :: [Verification]
  , stateDebts :: [Debt]
  , stateAudits :: [Audit]
  , stateClaims :: [Claim]
  , stateHolds :: Set HoldId
  , stateIntegrations :: [IntegrationRecord]
  , stateClosed :: Maybe Closure
  , stateIterating :: Bool
  }
  deriving (Eq, Ord, Show)

emptyState :: ChangeId -> ChangeState
emptyState change =
  ChangeState
    { stateChange = change
    , statePatchsets = []
    , stateVerdicts = []
    , stateFindings = []
    , stateDispositions = []
    , stateVerifications = []
    , stateDebts = []
    , stateAudits = []
    , stateClaims = []
    , stateHolds = Set.empty
    , stateIntegrations = []
    , stateClosed = Nothing
    , stateIterating = False
    }

-- | Fold events into state. Nothing here reads the clock, the filesystem, or
-- the Git repository.
replay :: ChangeId -> [Event] -> ChangeState
replay change = foldl step (emptyState change)
  where
    step state event = case event of
      PatchsetRecorded value -> state {statePatchsets = statePatchsets state <> [value]}
      VerdictRecorded value -> state {stateVerdicts = stateVerdicts state <> [value]}
      FindingRecorded value -> state {stateFindings = stateFindings state <> [value]}
      FindingDisposed value -> state {stateDispositions = stateDispositions state <> [value]}
      VerificationRecorded value -> state {stateVerifications = stateVerifications state <> [value]}
      DebtDeclared value -> state {stateDebts = stateDebts state <> [value]}
      AuditRecorded value -> state {stateAudits = stateAudits state <> [value]}
      ClaimStarted value -> state {stateClaims = stateClaims state <> [value]}
      ClaimExpired claim -> state {stateClaims = map (expire claim) (stateClaims state)}
      HoldSet hold -> state {stateHolds = Set.insert hold (stateHolds state)}
      HoldReleased hold -> state {stateHolds = Set.delete hold (stateHolds state)}
      ChangeClosed closure -> state {stateClosed = Just closure}
      IteratingChanged value -> state {stateIterating = value}
      IntegrationRecorded value -> state {stateIntegrations = stateIntegrations state <> [value]}
    expire claim value
      | claimId value == claim = value {claimExpired = True}
      | otherwise = value

latestPatchset :: ChangeState -> Maybe Patchset
latestPatchset state = case statePatchsets state of
  [] -> Nothing
  patchsets -> Just (last patchsets)

patchsetById :: ChangeState -> PatchsetId -> Maybe Patchset
patchsetById state identifier = case filter ((== identifier) . patchsetId) (statePatchsets state) of
  patchset : _ -> Just patchset
  [] -> Nothing

-- | Verdicts nothing supersedes. A corroborating verdict supports a tip
-- without replacing it.
activeVerdicts :: ChangeState -> [Verdict]
activeVerdicts ChangeState {..} =
  [ v
  | v <- stateVerdicts
  , not (any (supersedesSubject v) stateVerdicts)
  ]
  where
    supersedesSubject subject other =
      verdictRelation other == Supersedes && verdictSupersedes other == Just (verdictEvent subject)

-- | Two verdicts replacing the same earlier verdict fork the chain. No
-- verdict is authoritative until one supersedes them all, so the state is
-- contested rather than unreviewed.
verdictContested :: ChangeState -> Bool
verdictContested state = length (activeVerdicts state) > 1

governingVerdict :: ChangeState -> Maybe Verdict
governingVerdict state = case activeVerdicts state of
  [v] -> Just v
  _ -> Nothing

findingResolved :: ChangeState -> FindingId -> Bool
findingResolved ChangeState {..} identifier =
  any (\d -> dispositionFinding d == identifier && dispositionResolved d) stateDispositions

-- | Open blocking findings from the shipped review, never the audit set.
openBlockingFindings :: ChangeState -> [FindingId]
openBlockingFindings state@ChangeState {..} =
  [ findingId f
  | f <- stateFindings
  , findingBlocking f
  , not (findingAudit f)
  , not (findingResolved state (findingId f))
  ]

openAuditFindings :: ChangeState -> [FindingId]
openAuditFindings state@ChangeState {..} =
  [ findingId f
  | f <- stateFindings
  , findingAudit f
  , not (findingResolved state (findingId f))
  ]

debtsForPatchset :: ChangeState -> PatchsetId -> [Debt]
debtsForPatchset state identifier =
  [ debt
  | debt <- stateDebts state
  , debtPatchset debt == Just identifier
  ]

-- | The newest recorded integration, in recording order.
latestIntegration :: ChangeState -> Maybe IntegrationRecord
latestIntegration state = case stateIntegrations state of
  [] -> Nothing
  records -> Just (last (sortOn integratedEvent records))

-- | What shipped and what it rested on. A later review or audit never
-- changes this answer.
historicalAuthorization :: ChangeState -> Maybe Authorization
historicalAuthorization state = integratedAuthorization <$> latestIntegration state

activeIntegration :: ChangeState -> Maybe IntegrationRecord
activeIntegration = latestIntegration
