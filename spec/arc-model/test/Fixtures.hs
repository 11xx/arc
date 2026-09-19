{-# LANGUAGE RecordWildCards #-}

-- | Unit fixtures. Each one anchors a piece of intended meaning with an
-- exact expected answer, including the answer's shape: which refusal, which
-- authorization, which gate reading.
module Fixtures (fixtureChecks) where

import Arc.Model
import Generators
import Mutants (Behaviour (..), allMutants, mutantBehaviour, mutantName)
import Render

fixtureChecks :: [Check]
fixtureChecks =
  concat
    [ waiverExpiry
    , debtBesideRefusal
    , underDebtThenNegativeAudit
    , repairFollowedByDeeperReview
    , unknownCoverage
    , equalTreeDifferentContributors
    , retainedAcrossEpisodeExpiry
    , debtAuthorizedNothing
    , staleAndMovedBases
    , permissionIsNotEffect
    , auditRefusals
    , provisionalGates
    , demonstratedCounterexample
    ]

-- | A debt declared on ps-01 waives ps-01 only. A second patchset is a new
-- obligation, not a re-use.
waiverExpiry :: [Check]
waiverExpiry =
  [ expectPermittedWith
      "fixture/waiver-expiry: ps-01 is waived"
      (AuthorizedByWaiver (DebtId 1))
      (builtDecision (build scenario))
  , expectRefusedWith
      "fixture/waiver-expiry: ps-02 is not waived"
      "no-approval"
      (builtDecision (build scenario {scnPatchsets = 2}))
  ]
  where
    scenario = defaultScenario {scnReviewer = Nothing, scnDebt = Just (1, Nothing)}

-- | A changes-requested verdict beside debt and an open blocking finding is
-- not waived. The debt declares a missing review; it does not clear an
-- answer, and the finding is checked first.
debtBesideRefusal :: [Check]
debtBesideRefusal =
  [ expectTrue
      "fixture/debt-beside-refusal: refused"
      "a changes-requested verdict with an open finding must not be permitted"
      (not (isPermitted decision))
  , expectRefusedWith "fixture/debt-beside-refusal: finding blocks first" "blocking-findings" decision
  , expectTrue
      "fixture/debt-beside-refusal: debt authorized nothing"
      "the declared debt must not appear in any basis"
      (noBasisNamesDebt)
  , expectRefusedWith "fixture/debt-beside-refusal: refusal stands without the finding" "verdict-stands" decisionWithoutFinding
  ]
  where
    scenario = defaultScenario {scnReviewer = Just ActorIndependent, scnVerdict = ChangesRequested, scnDebt = Just (1, Nothing), scnBlockingFinding = True}
    decision = builtDecision (build scenario)
    decisionWithoutFinding = builtDecision (build scenario {scnBlockingFinding = False})
    noBasisNamesDebt = case decision of
      Permitted basis -> case basisAuthorization basis of
        AuthorizedByWaiver _ -> False
        AuthorizedByVerdictUnderWaiver _ _ -> False
        _ -> True
      Refused _ -> True

-- | Integration under debt, then a negative audit. The read is fulfilled,
-- the findings stay open, the merge basis is untouched, and fulfilled does
-- not become approved.
underDebtThenNegativeAudit :: [Check]
underDebtThenNegativeAudit =
  [ expectPermittedWith "fixture/under-debt: permitted on the waiver" (AuthorizedByWaiver (DebtId 1)) decision
  , expectEq
      "fixture/under-debt: basis survives the audit"
      (Just (AuthorizedByWaiver (DebtId 1)))
      (historicalAuthorization finalState)
  , expectEq "fixture/under-debt: read fulfilled by audit" (Just (ReadByAudit (EventId 901))) (coverageRead coverage)
  , expectEq "fixture/under-debt: verdict is the audit's" (Just ChangesRequested) (coverageVerdict coverage)
  , expectTrue "fixture/under-debt: fulfilled is not approved" "an audit must not approve by discharging a read" (not (coverageApproved coverage))
  , expectEq "fixture/under-debt: findings stay open" [FindingId 2] (coverageOpenFindings coverage)
  , expectEq "fixture/under-debt: authorization untouched" (Just (AuthorizedByWaiver (DebtId 1))) (coverageAuthorization coverage)
  ]
  where
    built = build defaultScenario {scnReviewer = Nothing, scnDebt = Just (1, Nothing), scnAudit = Just (ChangesRequested, True)}
    decision = builtDecision built
    finalState = builtFinalState built
    coverage = coverageAfterIntegration finalState

-- | An approved first patchset and a repair nobody read: the approval is
-- stale, and the owed review is a repair-unread, not nothing-read.
repairFollowedByDeeperReview :: [Check]
repairFollowedByDeeperReview =
  [ expectRefusedWith "fixture/repair-review: stale approval refused" "stale-approval" decision
  , expectEq "fixture/repair-review: deeper review owed" (OwedReview RepairUnread) (reviewObligation state)
  ]
  where
    built = build defaultScenario {scnPatchsets = 2, scnVerdictOnFirst = True, scnReviewer = Just ActorIndependent}
    decision = builtDecision built
    state = builtState built

-- | Omitted evidence is unknown: not false, not successful. Each way a gate
-- can fail to answer is its own reading and its own refusal.
unknownCoverage :: [Check]
unknownCoverage =
  [ expectEq "fixture/unknown: result omitted" Omitted (readingResult omitted)
  , expectEq "fixture/unknown: coverage never evaluated" NeverEvaluated (readingCoverage omitted)
  , expectEq "fixture/unknown: availability not produced" NotProduced (readingAvailability omitted)
  , expectEq "fixture/unknown: falsification omitted" Omitted (readingFalsified omitted)
  , expectTrue "fixture/unknown: omitted never permits" "omitted evidence must not be permitted" (not (isPermitted omittedDecision))
  , expectEq "fixture/unknown: gate refusal" (Left (GateNeverEvaluated gateName)) (gateGreen gateName (Just declaration) tree [])
  , expectEq "fixture/elsewhere: result observed" (Observed GatePass) (readingResult elsewhere)
  , expectEq "fixture/elsewhere: coverage elsewhere" (EvaluatedOtherTree (TreeId "tree-elsewhere")) (readingCoverage elsewhere)
  , expectRefusedWith "fixture/elsewhere: refused" "gates" elsewhereDecision
  , expectEq "fixture/changed: declaration moved" (DeclarationMoved (DeclarationId "build")) (readingCoverage changed)
  , expectRefusedWith "fixture/changed: refused" "gates" changedDecision
  , expectEq "fixture/unreadable: availability unreadable" EvidenceUnreadable (readingAvailability unreadable)
  , expectEq "fixture/unreadable: result omitted" Omitted (readingResult unreadable)
  , expectRefusedWith "fixture/unreadable: refused" "gates" unreadableDecision
  ]
  where
    gateName = GateName "build"
    declaration = Declaration (DeclarationId "build") "cargo build" 60
    tree = TreeId "tree1"
    omitted = readGate gateName declaration tree (stateVerifications (builtState (build defaultScenario {scnGateMode = GateOmitted})))
    omittedDecision = builtDecision (build defaultScenario {scnGateMode = GateOmitted})
    elsewhere = readGate gateName declaration tree (stateVerifications (builtState (build defaultScenario {scnGateMode = GateOtherTree})))
    elsewhereDecision = builtDecision (build defaultScenario {scnGateMode = GateOtherTree})
    changed = readGate gateName declaration tree (stateVerifications (builtState (build defaultScenario {scnGateMode = GateShapeMoved})))
    changedDecision = builtDecision (build defaultScenario {scnGateMode = GateShapeMoved})
    unreadable = readGate gateName declaration tree (stateVerifications (builtState (build defaultScenario {scnGateMode = GateUnreadable})))
    unreadableDecision = builtDecision (build defaultScenario {scnGateMode = GateUnreadable})

-- | Equal trees with different contributor and obligation scopes decide
-- differently. The tree alone says nothing.
equalTreeDifferentContributors :: [Check]
equalTreeDifferentContributors =
  [ expectRefusedWith "fixture/equal-tree: contributor reviewer refuses" "self-approval" contributorDecision
  , expectPermittedWith "fixture/equal-tree: independent reviewer permits" (AuthorizedByVerdict (EventId 2)) independentDecision
  , expectPermittedWith "fixture/equal-tree: waiver rescues the contributor" (AuthorizedByVerdictUnderWaiver (EventId 2) (DebtId 1)) waivedContributorDecision
  , expectTrue
      "fixture/equal-tree: same tree"
      "the two histories must share a tree"
      (patchsetTreeOf contributorState == patchsetTreeOf independentState)
  , expectTrue
      "fixture/equal-tree: debt unused beside approval"
      "an approval that needed no waiver must not name one"
      (debtsNotUsed independentState (AuthorizedByVerdict (EventId 2)) == [DebtId 1])
  ]
  where
    contributorBuilt = build defaultScenario {scnReviewer = Just ActorContributor}
    independentBuilt = build defaultScenario {scnReviewer = Just ActorIndependent}
    waivedContributorBuilt = build defaultScenario {scnReviewer = Just ActorContributor, scnDebt = Just (1, Nothing)}
    contributorDecision = builtDecision contributorBuilt
    independentDecision = builtDecision independentBuilt
    waivedContributorDecision = builtDecision waivedContributorBuilt
    contributorState = builtState contributorBuilt
    independentState = builtState (build defaultScenario {scnReviewer = Just ActorIndependent, scnDebt = Just (1, Nothing)})
    patchsetTreeOf state = maybe (TreeId "") patchsetTree (latestPatchset state)

-- | An expired liveness episode ends the claim, never the facts recorded
-- while it ran.
retainedAcrossEpisodeExpiry :: [Check]
retainedAcrossEpisodeExpiry =
  [ expectTrue "fixture/episode: claim expired" "the claim must be marked expired" (all claimExpired (stateClaims state))
  , expectTrue "fixture/episode: debt retained" "the debt must survive the expiry" (any ((== DebtId 1) . debtId) (stateDebts state))
  , expectTrue "fixture/episode: evidence retained" "the verification must survive the expiry" (not (null (stateVerifications state)))
  , expectPermittedWith "fixture/episode: waiver still applies" (AuthorizedByWaiver (DebtId 1)) (builtDecision built)
  ]
  where
    built = build defaultScenario {scnReviewer = Nothing, scnDebt = Just (1, Nothing), scnEpisodeExpired = True}
    state = builtState built

-- | A debt declared beside an approval that stood anyway authorized
-- nothing. It is recorded debt, not an authorization input.
debtAuthorizedNothing :: [Check]
debtAuthorizedNothing =
  [ expectPermittedWith "fixture/debt-unused: approval authorizes" (AuthorizedByVerdict (EventId 2)) decision
  , expectTrue "fixture/debt-unused: waiver not used" "the unused debt must not be named" (not (waiverUsed authorization (DebtId 1)))
  , expectEq "fixture/debt-unused: recorded as unused" [DebtId 1] (debtsNotUsed state authorization)
  ]
  where
    built = build defaultScenario {scnReviewer = Just ActorIndependent, scnDebt = Just (1, Nothing)}
    decision = builtDecision built
    state = builtState built
    authorization = case decision of
      Permitted basis -> basisAuthorization basis
      Refused _ -> AuthorizedByVerdict (EventId 0)

-- | A stale patchset is refused; a target or policy that moves between the
-- decision and the execution stands the action down rather than reusing the
-- basis.
staleAndMovedBases :: [Check]
staleAndMovedBases =
  [ expectRefusedWith "fixture/stale: head moved" "head-moved" staleDecision
  , expectTrue "fixture/target-moved: decision permits" "the decision is made against target-1" (isPermitted targetDecision)
  , expectEq
      "fixture/target-moved: execution stands down"
      (Left (RefusedBasisMoved [MovedTarget (Revision "target-1") (Revision "target-2")]))
      targetExecution
  , expectTrue "fixture/policy-moved: decision permits" "the decision is made under the danger policy" (isPermitted policyDecision)
  , expectEq
      "fixture/policy-moved: execution stands down"
      (Left (RefusedBasisMoved [MovedPolicy dangerPolicy openPolicy]))
      policyExecution
  ]
  where
    staleDecision = builtDecision (build defaultScenario {scnHeadMoved = True})
    targetBuilt = build defaultScenario {scnTargetAfter = True}
    targetDecision = builtDecision targetBuilt
    targetExecution = builtExecution targetBuilt
    policyBuilt = build defaultScenario {scnPolicyAfter = True}
    policyDecision = builtDecision policyBuilt
    policyExecution = builtExecution policyBuilt

-- | A permitted action is not an effect. The basis becomes history only when
-- the effect is recorded.
permissionIsNotEffect :: [Check]
permissionIsNotEffect =
  [ expectTrue "fixture/permission-not-effect: no integration without an effect" "permission alone must not record an integration" (historicalAuthorization state == Nothing)
  , case builtExecution built of
      Right plan ->
        expectEq
          "fixture/permission-not-effect: recording lands the basis"
          (Just (AuthorizedByVerdict (EventId 2)))
          (historicalAuthorization (recordIntegration (EventId 77) plan state))
      Left refusal -> failCheck "fixture/permission-not-effect: recording lands the basis" ("unexpected refusal: " <> refusalText refusal)
  ]
  where
    built = build defaultScenario
    state = builtState built

-- | Audit gating: open changes refuse one; an approving audit needs a
-- declared independent identity; a negative audit is open to anyone.
auditRefusals :: [Check]
auditRefusals =
  [ expectAuditRefusal "fixture/audit: open change refuses" AuditWhileOpen (auditDischarges openState debt openAudit)
  , expectAuditRefusal "fixture/audit: assumed approver refuses" AuditAssumedAuditor (auditDischarges closedState debt (approving {auditAssumed = True}))
  , expectAuditRefusal "fixture/audit: contributor approver refuses" AuditAuditorNotIndependent (auditDischarges closedState debt (approving {auditActor = authorActor}))
  , expectTrue
      "fixture/audit: independent approver discharges"
      "an independent approving audit must discharge the read"
      (case auditDischarges closedState debt independentApproving of
         Right discharge -> dischargeApproves discharge && dischargeRead discharge == ReadByAudit (auditEvent independentApproving)
         Left _ -> False)
  , expectTrue
      "fixture/audit: negative audit is open to anyone"
      "a negative audit needs no independence"
      (case auditDischarges closedState debt negativeByAuthor of
         Right discharge -> not (dischargeApproves discharge)
         Left _ -> False)
  ]
  where
    debt = Debt (DebtId 9) (EventId 900) (Just (PatchsetId 1)) Nothing "owed review" authorActor
    openBuilt = build defaultScenario {scnVerdict = ChangesRequested}
    openState = builtState openBuilt
    closedBuilt = build defaultScenario {scnAudit = Just (Approved, True)}
    closedState = builtFinalState closedBuilt
    openAudit = Audit (EventId 950) (Revision "rev1") ChangesRequested otherActor False []
    approving = Audit (EventId 951) (Revision "rev1") Approved otherActor False []
    independentApproving = approving
    negativeByAuthor = approving {auditKind = ChangesRequested, auditActor = authorActor}
    expectAuditRefusal name expected result = case result of
      Left refusal
        | refusal == expected -> passCheck name (auditRefusalText refusal)
        | otherwise -> failCheck name ("refused as " <> show refusal <> ", expected " <> show expected)
      Right discharge -> failCheck name ("discharged as " <> show discharge)

-- | A provisional approval gates like any other; corroboration is owed, not
-- required.
provisionalGates :: [Check]
provisionalGates =
  [ expectPermittedWith "fixture/provisional: gates" (AuthorizedByVerdict (EventId 2)) (builtDecision (build defaultScenario {scnProvisional = True}))
  ]

-- | The demonstrated counterexample the report cites: one deliberate fault,
-- one minimal history, one exact divergence.
demonstratedCounterexample :: [Check]
demonstratedCounterexample =
  [ expectRefusedWith "fixture/demonstration: model refuses the contributor reviewer" "self-approval" specDecision
  , expectTrue
      "fixture/demonstration: fault ignored contributor identity"
      "the contributor-identity-ignored mutant must permit where the model refuses"
      (isPermitted mutantDecision)
  ]
  where
    built = build defaultScenario {scnReviewer = Just ActorContributor}
    specDecision = builtDecision built
    mutant = case filter ((== "contributor-identity-ignored") . mutantName) allMutants of
      found : _ -> found
      [] -> error "the contributor-identity-ignored mutant is missing"
    mutantDecision = case mutantBehaviour mutant built of
      BehaviourDecision decision -> decision
      _ -> Refused RefusedNoApproval
