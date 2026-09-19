{-# LANGUAGE RecordWildCards #-}

-- | Coverage obligations: what review is owed, and whether it was supplied.
--
-- Coverage obligation, verdict outcome, and historical authorization are
-- separate answers. A fulfilled read is not an approval, an approval is not
-- an effect, and neither is rewritten by later work.
module Arc.Model.Debt
  ( debtKindFor
  , ReadEvidence (..)
  , ReviewObligation (..)
  , CoverageAfterIntegration (..)
  , reviewObligation
  , coverageAfterIntegration
  , waiverUsed
  , debtsNotUsed
  , authorizationDebts
  , auditIsIndependent
  ) where

import Data.Maybe (isJust)
import qualified Data.Set as Set

import Arc.Model.Decision (newestWaiver)
import Arc.Model.History
import Arc.Model.Identifiers

-- | What kind of deficit a debt names. A declared kind wins; otherwise the
-- ledger derives one from the verdicts recorded on the shipped patchset.
-- Only a caller can say a merge resolution or a repair is what went unread,
-- so those two kinds are never derived.
debtKindFor :: ChangeState -> Debt -> DebtKind
debtKindFor state debt = case debtDeclaredKind debt of
  Just kind -> kind
  Nothing -> case debtPatchset debt >>= patchsetById state of
    Nothing -> NothingRead
    Just patchset ->
      case filter ((== patchsetId patchset) . verdictPatchset) (stateVerdicts state) of
        [] -> NothingRead
        verdicts ->
          if all ((`Set.member` effectiveContributors patchset) . verdictEffectiveActor) verdicts
            then ContributorOnly
            else IndependentReview

-- | A read somebody supplied.
data ReadEvidence
  = ReadByVerdict EventId
  | ReadByAudit EventId
  deriving (Eq, Ord, Show)

-- | What review the change owes right now.
data ReviewObligation
  = CoveringRead ReadEvidence
  | ReviewWaived DebtKind DebtId
  | OwedReview DebtKind
  | StandingRefusal VerdictKind EventId [FindingId]
  deriving (Eq, Ord, Show)

-- | The projection a lead reads: is there a covering read, a standing
-- refusal, or an owed review, and of what kind.
reviewObligation :: ChangeState -> ReviewObligation
reviewObligation state = case (governingVerdict state, latestPatchset state) of
  (Just verdict, Just patchset)
    | verdictKind verdict == Approved && verdictPatchset verdict == patchsetId patchset ->
        CoveringRead (ReadByVerdict (verdictEvent verdict))
    | verdictKind verdict /= Approved && verdictPatchset verdict == patchsetId patchset ->
        StandingRefusal (verdictKind verdict) (verdictEvent verdict) (openBlockingFindings state)
  (_, Just patchset) -> case newestWaiver state (patchsetId patchset) of
    Just debt -> ReviewWaived (debtKindFor state debt) (debtId debt)
    Nothing -> OwedReview (owedKind state)
  _ -> OwedReview NothingRead
  where
    owedKind current = case stateVerdicts current of
      [] -> NothingRead
      verdicts
        | any ((== ChangesRequested) . verdictKind) verdicts -> IndependentReview
        | otherwise -> RepairUnread

-- | The post-integration projection. The read requirement, the verdict the
-- audit returned, and the authorization the merge actually rested on are
-- three separate fields, and an open audit finding never disappears because
-- the read was satisfied.
data CoverageAfterIntegration = CoverageAfterIntegration
  { coverageRead :: Maybe ReadEvidence
  , coverageDebt :: Maybe DebtId
  , coverageVerdict :: Maybe VerdictKind
  , coverageApproved :: Bool
  , coverageOpenFindings :: [FindingId]
  , coverageAuthorization :: Maybe Authorization
  }
  deriving (Eq, Ord, Show)

coverageAfterIntegration :: ChangeState -> CoverageAfterIntegration
coverageAfterIntegration state = case latestIntegration state of
  Nothing ->
    CoverageAfterIntegration
      { coverageRead = Nothing
      , coverageDebt = Nothing
      , coverageVerdict = Nothing
      , coverageApproved = False
      , coverageOpenFindings = []
      , coverageAuthorization = Nothing
      }
  Just record ->
    CoverageAfterIntegration
      { coverageRead = readEvidence
      , coverageDebt = debtUsed record
      , coverageVerdict = latestAuditVerdict
      , coverageApproved = independentApproval
      , coverageOpenFindings = openAuditFindings state
      , coverageAuthorization = Just (integratedAuthorization record)
      }
  where
    debtUsed record = case integratedAuthorization record of
      AuthorizedByWaiver debt -> Just debt
      AuthorizedByVerdictUnderWaiver _ debt -> Just debt
      _ -> Nothing
    independentVerdict = do
      record <- latestIntegration state
      event <- case integratedAuthorization record of
        AuthorizedByVerdict event -> Just event
        _ -> Nothing
      verdict <- verdictByEvent state event
      verdictIsIndependent state verdict >> Just event
    auditRead = do
      record <- latestIntegration state
      let audits =
            [ a
            | a <- stateAudits state
            , auditRevision a == integratedHead record
            , auditIsIndependent state a
            ]
      case audits of
        [] -> Nothing
        _ -> Just (ReadByAudit (auditEvent (last audits)))
    readEvidence = case independentVerdict of
      Just event -> Just (ReadByVerdict event)
      Nothing -> auditRead
    latestAuditVerdict = case stateAudits state of
      [] -> Nothing
      audits -> Just (auditKind (last audits))
    independentApproval =
      any (\a -> auditKind a == Approved && auditIsIndependent state a) (stateAudits state)
        || isJust independentVerdict

verdictByEvent :: ChangeState -> EventId -> Maybe Verdict
verdictByEvent state event = case [v | v <- stateVerdicts state, verdictEvent v == event] of
  verdict : _ -> Just verdict
  [] -> Nothing

verdictIsIndependent :: ChangeState -> Verdict -> Maybe ()
verdictIsIndependent state verdict = do
  patchset <- patchsetById state (verdictPatchset verdict)
  if verdictAssumed verdict || verdictEffectiveActor verdict `Set.member` effectiveContributors patchset
    then Nothing
    else Just ()

-- | A read recorded after integration is independent when its identity was
-- declared and it is not one of the shipped patchset's contributors. The
-- same predicate the audit gate consults, so a projection and a refusal
-- cannot disagree about independence.
auditIsIndependent :: ChangeState -> Audit -> Bool
auditIsIndependent state audit = case latestIntegration state >>= patchsetById state . integratedPatchset of
  Nothing -> False
  Just patchset ->
    not (auditAssumed audit)
      && auditActor audit `Set.notMember` effectiveContributors patchset

-- | Whether an authorization actually consumed this debt. A debt recorded
-- beside a standing approval authorized nothing and is reported as recorded
-- debt, not as an authorization input.
waiverUsed :: Authorization -> DebtId -> Bool
waiverUsed authorization debt = debt `elem` authorizationDebts authorization

-- | Every debt declaration that the given authorization did not consume.
debtsNotUsed :: ChangeState -> Authorization -> [DebtId]
debtsNotUsed state authorization =
  [ debtId debt
  | debt <- stateDebts state
  , not (waiverUsed authorization (debtId debt))
  ]

-- | The debt declarations an authorization named.
authorizationDebts :: Authorization -> [DebtId]
authorizationDebts authorization = case authorization of
  AuthorizedByVerdict _ -> []
  AuthorizedByWaiver debt -> [debt]
  AuthorizedByVerdictUnderWaiver _ debt -> [debt]
