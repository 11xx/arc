{-# LANGUAGE RecordWildCards #-}

-- | Validation of a requested action.
--
-- 'decide' reads a replayed history and the observations, and answers with
-- either a basis naming the facts it relied on or a structured refusal.
-- 'execute' re-checks that the basis still holds before producing a plan;
-- 'recordIntegration' is the separate step that puts the effect in the
-- ledger. A permission alone never records anything.
module Arc.Model.Decision
  ( decide
  , authorizationFor
  , gateEvidence
  , newestWaiver
  , execute
  , ExecutionPlan (..)
  , recordIntegration
  ) where

import qualified Data.Set as Set

import Arc.Model.Basis
import Arc.Model.History
import Arc.Model.Identifiers
import Arc.Model.Observation

-- | Decide whether an integration is permitted now.
decide :: Observations -> ChangeState -> Decision
decide observations state
  | Just closure <- stateClosed state = Refused (RefusedClosed closure)
  | stateIterating state = Refused RefusedIterating
  | not (null (obsBlockedBy observations)) = Refused (RefusedBlockedBy (obsBlockedBy observations))
  | otherwise = case latestPatchset state of
      Nothing -> Refused RefusedNoPatchset
      Just patchset
        | obsHead observations /= patchsetRevision patchset ->
            Refused (RefusedHeadMoved (obsHead observations) (patchsetRevision patchset))
        | not (null openFindings) -> Refused (RefusedBlockingFindings openFindings)
        | verdictContested state ->
            Refused (RefusedContestedVerdict (map verdictEvent (activeVerdicts state)))
        | policyRequireDeclaredActor policy && not (obsInvokerDeclared observations) ->
            Refused RefusedUndeclaredActor
        | otherwise -> case authorizationFor policy state patchset of
            Left refusal -> Refused refusal
            Right authorization -> case gateEvidence observations state of
              Left refusals -> Refused (RefusedGates refusals)
              Right gates -> case Set.lookupMin (stateHolds state) of
                Just hold -> Refused (RefusedHoldActive hold)
                Nothing ->
                  Permitted
                    DecisionBasis
                      { basisPatchset = patchsetId patchset
                      , basisHead = patchsetRevision patchset
                      , basisTree = obsEvaluatedTree observations
                      , basisTargetBranch = obsTargetBranch observations
                      , basisTarget = obsTarget observations
                      , basisPolicy = policy
                      , basisAuthorization = authorization
                      , basisGates = gates
                      , basisConsumedFindings = openFindings
                      , basisConsumedHolds = []
                      }
  where
    openFindings = openBlockingFindings state
    policy = obsPolicy observations

-- | The recorded approval or waiver that lets this patchset stand.
authorizationFor :: Policy -> ChangeState -> Patchset -> Either Refusal Authorization
authorizationFor policy state patchset =
  case governingVerdict state of
    Nothing -> case waiver of
      Just debt -> Right (AuthorizedByWaiver (debtId debt))
      Nothing -> Left RefusedNoApproval
    Just verdict
      | verdictKind verdict == Approved && verdictPatchset verdict == patchsetId patchset ->
          if selfApprovalRejected verdict
            then case waiver of
              Just debt ->
                Right (AuthorizedByVerdictUnderWaiver (verdictEvent verdict) (debtId debt))
              Nothing ->
                Left
                  ( RefusedSelfApproval
                      (verdictEvent verdict)
                      (verdictEffectiveActor verdict)
                      (effectiveContributors patchset)
                  )
            else Right (AuthorizedByVerdict (verdictEvent verdict))
      | verdictKind verdict /= Approved && verdictPatchset verdict == patchsetId patchset ->
          -- A refusal on the current patchset is the action itself: a waiver
          -- declares a missing review, it does not clear an answer.
          Left (RefusedVerdictStands (verdictKind verdict) (verdictEvent verdict))
      | otherwise -> case waiver of
          Just debt -> Right (AuthorizedByWaiver (debtId debt))
          Nothing -> case verdictKind verdict of
            Approved -> Left (RefusedStaleApproval (verdictEvent verdict) (verdictPatchset verdict))
            _ -> Left RefusedNoApproval
  where
    waiver = newestWaiver state (patchsetId patchset)
    selfApprovalRejected verdict =
      policyIndependentVerdictRequired policy
        && policyForbidSelfApproval policy
        && ( verdictAssumed verdict
               || verdictEffectiveActor verdict `Set.member` effectiveContributors patchset
           )

-- | The newest debt whose waiver binds to exactly this patchset. Later
-- declarations for the same patchset win; a declaration for any other
-- patchset waives nothing here.
newestWaiver :: ChangeState -> PatchsetId -> Maybe Debt
newestWaiver state patchset =
  case debtsForPatchset state patchset of
    [] -> Nothing
    debts -> Just (last debts)

-- | Read every required gate against the evaluated tree. A gate that is
-- required but not declared is refused like any other missing evidence.
gateEvidence :: Observations -> ChangeState -> Either [GateRefusal] [(GateName, EventId, DeclarationId)]
gateEvidence Observations {..} state =
  case [refusal | (_, _, Left refusal) <- results] of
    [] -> Right [evidence | Just evidence <- map toEvidence results]
    refusals -> Left refusals
  where
    results =
      [ (gate, declaration, gateGreen gate declaration obsEvaluatedTree (stateVerifications state))
      | (gate, declarationIdWanted) <- obsRequiredGates
      , let declaration = lookupDeclaration declarationIdWanted
      ]
    lookupDeclaration wanted =
      case [d | d <- obsDeclarations, declarationId d == wanted] of
        declaration : _ -> Just declaration
        [] -> Nothing
    toEvidence (gate, Just declaration, Right reading) = case readingCoverage reading of
      Covered event -> Just (gate, event, declarationId declaration)
      _ -> Nothing
    toEvidence _ = Nothing

-- | Re-check a basis against the observations at execution time. Any moved
-- fact stands the action down; the recorded basis is never reused.
execute :: Observations -> ChangeState -> Decision -> Either Refusal ExecutionPlan
execute observations state decision = case decision of
  Refused refusal -> Left refusal
  Permitted basis ->
    case moved of
      [] -> Right ExecutionPlan {planIntegration = integration}
      facts -> Left (RefusedBasisMoved facts)
    where
      moved =
        concat
          [ [MovedHead (basisHead basis) (obsHead observations) | basisHead basis /= obsHead observations]
          , [MovedTarget (basisTarget basis) (obsTarget observations) | basisTarget basis /= obsTarget observations]
          , [MovedTree (basisTree basis) (obsEvaluatedTree observations) | basisTree basis /= obsEvaluatedTree observations]
          , [MovedPolicy (basisPolicy basis) (obsPolicy observations) | basisPolicy basis /= obsPolicy observations]
          , case latestPatchset state of
              Just patchset
                | patchsetId patchset /= basisPatchset basis ->
                    [MovedPatchset (basisPatchset basis) (patchsetId patchset)]
              _ -> []
          ]
      integration =
        IntegrationRecord
          { integratedEvent = EventId 0
          , integratedPatchset = basisPatchset basis
          , integratedHead = basisHead basis
          , integratedTargetBranch = basisTargetBranch basis
          , integratedTargetBefore = basisTarget basis
          , integratedTree = basisTree basis
          , integratedAuthorization = basisAuthorization basis
          , integratedGates = basisGates basis
          , integratedConsumedFindings = basisConsumedFindings basis
          , integratedConsumedHolds = basisConsumedHolds basis
          , integratedPolicy = basisPolicy basis
          }

-- | The plan an execution produced. Holding one proves nothing about the
-- world; only 'recordIntegration' puts it in the ledger.
newtype ExecutionPlan = ExecutionPlan {planIntegration :: IntegrationRecord}
  deriving (Eq, Ord, Show)

-- | Record the effect. An integration event carries the plan's basis, so a
-- later review can never rewrite what the merge relied upon.
recordIntegration :: EventId -> ExecutionPlan -> ChangeState -> ChangeState
recordIntegration event plan state =
  state {stateIntegrations = stateIntegrations state <> [record]}
  where
    record = (planIntegration plan) {integratedEvent = event}
