{-# LANGUAGE RecordWildCards #-}

-- | Scenarios: a compact spec of a history plus its observations.
--
-- A scenario is a plan, not a ledger. 'build' interprets it into events with
-- coherent references, so shrinking can drop features without ever leaving a
-- dangling identifier: the causal references are regenerated, not edited.
module Generators
  ( Scenario (..)
  , ActorPick (..)
  , GateMode (..)
  , defaultScenario
  , dangerPolicy
  , openPolicy
  , requireDeclaredPolicy
  , authorActor
  , reviewActor
  , otherActor
  , Built (..)
  , build
  , observationsFor
  , executionObservations
  , genAnyScenario
  , genIntegratable
  , isIntegratable
  , shrinkScenario
  , mutations
  , Mutation (..)
  , Feature (..)
  , allFeatures
  , featureOf
  ) where

import qualified Data.Set as Set
import Test.QuickCheck hiding (replay)

import Arc.Model

-- | Which identity records the verdict.
data ActorPick = ActorIndependent | ActorContributor | ActorAssumed
  deriving (Eq, Ord, Show, Enum, Bounded)

-- | How the required gate's evidence relates to the declaration and tree in
-- force.
data GateMode = GateCovered | GateOtherTree | GateShapeMoved | GateUnreadable | GateOmitted
  deriving (Eq, Ord, Show, Enum, Bounded)

-- | A compact plan for one history.
data Scenario = Scenario
  { scnPatchsets :: Int
  , scnVerdictOnFirst :: Bool
  , scnReviewer :: Maybe ActorPick
  , scnVerdict :: VerdictKind
  , scnProvisional :: Bool
  , scnExtraContributor :: Bool
  , scnDebt :: Maybe (Int, Maybe DebtKind)
  , scnGateMode :: GateMode
  , scnBlockingFinding :: Bool
  , scnResolveFinding :: Bool
  , scnHeadMoved :: Bool
  , scnPolicy :: Policy
  , scnTargetAfter :: Bool
  , scnPolicyAfter :: Bool
  , scnAudit :: Maybe (VerdictKind, Bool)
  , scnEpisodeExpired :: Bool
  }
  deriving (Eq, Ord, Show)

defaultScenario :: Scenario
defaultScenario =
  Scenario
    { scnPatchsets = 1
    , scnVerdictOnFirst = False
    , scnReviewer = Just ActorIndependent
    , scnVerdict = Approved
    , scnProvisional = False
    , scnExtraContributor = False
    , scnDebt = Nothing
    , scnGateMode = GateCovered
    , scnBlockingFinding = False
    , scnResolveFinding = False
    , scnHeadMoved = False
    , scnPolicy = dangerPolicy
    , scnTargetAfter = False
    , scnPolicyAfter = False
    , scnAudit = Nothing
    , scnEpisodeExpired = False
    }

dangerPolicy :: Policy
dangerPolicy =
  Policy
    { policyIndependentVerdictRequired = True
    , policyForbidSelfApproval = True
    , policyRequireDeclaredActor = False
    }

openPolicy :: Policy
openPolicy =
  Policy
    { policyIndependentVerdictRequired = False
    , policyForbidSelfApproval = False
    , policyRequireDeclaredActor = False
    }

requireDeclaredPolicy :: Policy
requireDeclaredPolicy = dangerPolicy {policyRequireDeclaredActor = True}

authorActor, reviewActor, otherActor :: ActorId
authorActor = ActorId "author"
reviewActor = ActorId "reviewer"
otherActor = ActorId "other"

scenarioChange :: ChangeId
scenarioChange = ChangeId "demo"

-- | What one scenario yields: the ledger, the observations, and the
-- decisions those two produce.
data Built = Built
  { builtEvents :: [Event]
  , builtFinalEvents :: [Event]
  , builtObservation :: Observations
  , builtExecutionObservation :: Observations
  , builtState :: ChangeState
  , builtFinalState :: ChangeState
  , builtDecision :: Decision
  , builtExecution :: Either Refusal ExecutionPlan
  }

build :: Scenario -> Built
build Scenario {..} =
  Built
    { builtEvents = events
    , builtFinalEvents = finalEvents
    , builtObservation = observations
    , builtExecutionObservation = executionObs
    , builtState = state
    , builtFinalState = replay scenarioChange finalEvents
    , builtDecision = decide observations state
    , builtExecution = execute executionObs state (decide observations state)
    }
  where
    patchsets =
      [ Patchset
          { patchsetId = PatchsetId index
          , patchsetOrdinal = index
          , patchsetRevision = Revision ("rev" <> show index)
          , patchsetTree = TreeId ("tree" <> show index)
          , patchsetAuthor = authorActor
          , patchsetContributors =
              if scnExtraContributor
                then Set.fromList [authorActor, otherActor]
                else Set.singleton authorActor
          }
      | index <- [1 .. scnPatchsets]
      ]
    latest = last patchsets
    verdictTarget =
      if scnVerdictOnFirst && scnPatchsets > 1
        then PatchsetId 1
        else patchsetId latest
    verdictEvent =
      [ VerdictRecorded
          Verdict
            { verdictEvent = EventId 0
            , verdictPatchset = verdictTarget
            , verdictKind = scnVerdict
            , verdictActor = actorForPick pick
            , verdictOnBehalfOf = Nothing
            , verdictAssumed = pick == ActorAssumed
            , verdictProvisional = if scnProvisional then Just "provisional" else Nothing
            , verdictRelation = Supersedes
            , verdictSupersedes = Nothing
            }
      | pick <- maybe [] pure scnReviewer
      ]
    findingEvents
      | scnBlockingFinding =
          FindingRecorded
            Finding
              { findingEvent = EventId 0
              , findingId = FindingId 1
              , findingPatchset = verdictTarget
              , findingActor = otherActor
              , findingBlocking = True
              , findingAudit = False
              }
            : [ FindingDisposed
                  Disposition
                    { dispositionEvent = EventId 0
                    , dispositionFinding = FindingId 1
                    , dispositionResolved = True
                    }
              | scnResolveFinding
              ]
      | otherwise = []
    debtEvents =
      [ DebtDeclared
          Debt
            { debtId = DebtId 1
            , debtEvent = EventId 0
            , debtPatchset = Just (PatchsetId index)
            , debtDeclaredKind = kind
            , debtReason = "declared coverage"
            , debtActor = authorActor
            }
      | (index, kind) <- maybe [] pure scnDebt
      ]
    verificationEvents = case scnGateMode of
      GateOmitted -> []
      mode ->
        [ VerificationRecorded
            Verification
              { verificationEvent = EventId 0
              , verificationGate = GateName "build"
              , verificationDeclaration = DeclarationId "build"
              , verificationShape = shape
              , verificationTree = tree
              , verificationResult = GatePass
              , verificationExecution = RanLocally
              , verificationAnswers = Just (FailureLabel "known-failure")
              , verificationReadable = readable
              }
        ]
        where
          shape = case mode of
            GateShapeMoved -> DeclarationShape "cargo build --locked" 60
            _ -> DeclarationShape "cargo build" 60
          tree = case mode of
            GateOtherTree -> TreeId "tree-elsewhere"
            _ -> patchsetTree latest
          readable = mode /= GateUnreadable
    claimEvents =
      concat
        [ [ ClaimStarted (Claim (ClaimId 1) authorActor False)
          , ClaimExpired (ClaimId 1)
          ]
        | scnEpisodeExpired
        ]
    events =
      assignIds
        ( map PatchsetRecorded patchsets
            <> verdictEvent
            <> findingEvents
            <> debtEvents
            <> verificationEvents
            <> claimEvents
        )
    observations = observationsForOf patchsets scnHeadMoved scnPolicy
    executionObs = observations {obsTarget = executionTarget, obsPolicy = executionPolicy}
    executionTarget = if scnTargetAfter then Revision "target-2" else obsTarget observations
    executionPolicy = if scnPolicyAfter then flipPolicy (obsPolicy observations) else obsPolicy observations
    state = replay scenarioChange events
    decision = decide observations state
    effectEvents = case decision of
      Permitted _
        | Right plan <- execute executionObs state decision ->
            [IntegrationRecorded ((planIntegration plan) {integratedEvent = EventId 900})]
              <> auditEvents
      _ -> auditEvents
    auditEvents = case scnAudit of
      Nothing -> []
      Just (kind, independent) ->
        [ AuditRecorded
            Audit
              { auditEvent = EventId 901
              , auditRevision = patchsetRevision latest
              , auditKind = kind
              , auditActor = if independent then otherActor else authorActor
              , auditAssumed = False
              , auditFindings = [FindingId 2 | kind == ChangesRequested]
              }
        ]
          <> [ FindingRecorded
                 Finding
                   { findingEvent = EventId 902
                   , findingId = FindingId 2
                   , findingPatchset = patchsetId latest
                   , findingActor = if independent then otherActor else authorActor
                   , findingBlocking = True
                   , findingAudit = True
                   }
             | kind == ChangesRequested
             ]
    finalEvents = events <> effectEvents

observationsFor :: Scenario -> Observations
observationsFor Scenario {..} =
  observationsForOf
    [ Patchset
        { patchsetId = PatchsetId index
        , patchsetOrdinal = index
        , patchsetRevision = Revision ("rev" <> show index)
        , patchsetTree = TreeId ("tree" <> show index)
        , patchsetAuthor = authorActor
        , patchsetContributors = Set.singleton authorActor
        }
    | index <- [1 .. scnPatchsets]
    ]
    scnHeadMoved
    scnPolicy

observationsForOf :: [Patchset] -> Bool -> Policy -> Observations
observationsForOf patchsets headMoved policy =
  Observations
    { obsChange = scenarioChange
    , obsHead = if headMoved then Revision "rev-moved" else patchsetRevision latest
    , obsTargetBranch = TargetBranch "main"
    , obsTarget = Revision "target-1"
    , obsEvaluatedTree = patchsetTree latest
    , obsDeclarations = [Declaration (DeclarationId "build") "cargo build" 60]
    , obsRequiredGates = [(GateName "build", DeclarationId "build")]
    , obsPolicy = policy
    , obsBlockedBy = []
    , obsInvokerDeclared = True
    }
  where
    latest = last patchsets

executionObservations :: Scenario -> Observations
executionObservations scenario =
  let base = observationsFor scenario
   in base
        { obsTarget = if scnTargetAfter scenario then Revision "target-2" else obsTarget base
        , obsPolicy = if scnPolicyAfter scenario then flipPolicy (obsPolicy base) else obsPolicy base
        }

-- | The policy a moved observation lands on: whichever declared policy is
-- not the one the decision rested on.
flipPolicy :: Policy -> Policy
flipPolicy policy = if policy == openPolicy then dangerPolicy else openPolicy

actorForPick :: ActorPick -> ActorId
actorForPick ActorIndependent = reviewActor
actorForPick ActorContributor = authorActor
actorForPick ActorAssumed = reviewActor

-- | Assign one event id per recorded event, in recording order.
assignIds :: [Event] -> [Event]
assignIds = go 1
  where
    go _ [] = []
    go n (event : rest) = withId (EventId n) event : go (n + 1) rest
    withId eventId event = case event of
      PatchsetRecorded value -> PatchsetRecorded value
      VerdictRecorded value -> VerdictRecorded value {verdictEvent = eventId}
      FindingRecorded value -> FindingRecorded value {findingEvent = eventId}
      FindingDisposed value -> FindingDisposed value {dispositionEvent = eventId}
      VerificationRecorded value -> VerificationRecorded value {verificationEvent = eventId}
      DebtDeclared value -> DebtDeclared value {debtEvent = eventId}
      AuditRecorded value -> AuditRecorded value {auditEvent = eventId}
      ClaimStarted value -> ClaimStarted value
      ClaimExpired value -> ClaimExpired value
      HoldSet value -> HoldSet value
      HoldReleased value -> HoldReleased value
      ChangeClosed value -> ChangeClosed value
      IteratingChanged value -> IteratingChanged value
      IntegrationRecorded value -> IntegrationRecorded value {integratedEvent = eventId}

-- | A generator that reaches every feature the suite claims to exercise.
genAnyScenario :: Gen Scenario
genAnyScenario = do
  scnPatchsets <- choose (1, 3)
  scnVerdictOnFirst <- frequency [(2, pure False), (1, pure True)]
  scnReviewer <- frequency [(1, pure Nothing), (3, Just <$> arbitrary), (2, pure (Just ActorIndependent))]
  scnVerdict <- elements [Approved, Approved, ChangesRequested, CommentOnly]
  scnProvisional <- frequency [(4, pure False), (1, pure True)]
  scnExtraContributor <- arbitrary
  scnDebt <- frequency [(2, pure Nothing), (2, debtFor scnPatchsets)]
  scnGateMode <- arbitrary
  scnBlockingFinding <- frequency [(3, pure False), (1, pure True)]
  scnResolveFinding <- frequency [(4, pure False), (1, pure True)]
  scnHeadMoved <- frequency [(4, pure False), (1, pure True)]
  scnPolicy <- elements [dangerPolicy, dangerPolicy, openPolicy, requireDeclaredPolicy]
  scnTargetAfter <- frequency [(4, pure False), (1, pure True)]
  scnPolicyAfter <- frequency [(4, pure False), (1, pure True)]
  scnAudit <- frequency [(2, pure Nothing), (1, Just <$> ((,) <$> elements [Approved, ChangesRequested] <*> arbitrary))]
  scnEpisodeExpired <- arbitrary
  pure Scenario {..}

debtFor :: Int -> Gen (Maybe (Int, Maybe DebtKind))
debtFor count = do
  index <- choose (1, count)
  kind <-
    frequency
      [ (1, pure Nothing)
      , (1, Just <$> elements [NothingRead, MergeResolutionUnread, RepairUnread, ContributorOnly, IndependentReview])
      ]
  pure (Just (index, kind))

-- | A generator restricted to histories an integration would permit. These
-- are the histories a one-invalid-transition mutation is applied to.
genIntegratable :: Gen Scenario
genIntegratable = do
  count <- choose (1, 3)
  withApproval <- frequency [(3, pure True), (1, pure False)]
  scnExtraContributor <- arbitrary
  scnDebt <- frequency [(2, pure Nothing), (2, debtFor count)]
  scnProvisional <- frequency [(4, pure False), (1, pure True)]
  scnEpisodeExpired <- arbitrary
  scnAudit <- frequency [(2, pure Nothing), (1, Just <$> ((,) <$> elements [Approved, ChangesRequested] <*> pure True))]
  let scnReviewer = if withApproval then Just ActorIndependent else Nothing
      waiver = if withApproval then scnDebt else Just (count, Nothing)
  pure
    defaultScenario
      { scnPatchsets = count
      , scnReviewer = scnReviewer
      , scnExtraContributor = scnExtraContributor
      , scnDebt = waiver
      , scnProvisional = scnProvisional
      , scnEpisodeExpired = scnEpisodeExpired
      , scnAudit = scnAudit
      , scnPolicy = dangerPolicy
      }

-- | Histories an integration would permit: an independent approval, or a
-- waiver bound to the latest patchset; covered gate evidence; no open
-- finding, no moved head, no verdict bound to an older patchset.
isIntegratable :: Scenario -> Bool
isIntegratable scenario =
  ( case scnReviewer scenario of
      Just ActorIndependent -> scnVerdict scenario == Approved
      Just _ -> False
      Nothing -> scnDebt scenario /= Nothing
  )
    && scnGateMode scenario == GateCovered
    && not (scnBlockingFinding scenario)
    && not (scnHeadMoved scenario)
    && not (scnVerdictOnFirst scenario && scnPatchsets scenario > 1)

instance Arbitrary Scenario where
  arbitrary = genAnyScenario
  shrink = shrinkScenario

instance Arbitrary ActorPick where
  arbitrary = elements [minBound .. maxBound]
  shrink value = [candidate | candidate <- [minBound .. value], candidate /= value]

instance Arbitrary GateMode where
  arbitrary = elements [minBound .. maxBound]
  shrink value = [candidate | candidate <- [minBound .. value], candidate /= value]

-- | Structural shrinking. References are positions, so a shrink that would
-- leave a reference dangling is filtered out rather than edited: every
-- candidate keeps the causal references its events need.
shrinkScenario :: Scenario -> [Scenario]
shrinkScenario scenario =
  [ candidate
  | candidate <- candidates
  , all (<= scnPatchsets candidate) (referencedPatchsets candidate)
  ]
  where
    candidates =
      [scenario {scnPatchsets = count} | count <- [1 .. scnPatchsets scenario - 1]]
        <> [scenario {scnVerdictOnFirst = False} | scnVerdictOnFirst scenario]
        <> [scenario {scnReviewer = Nothing} | scnReviewer scenario /= Nothing]
        <> [scenario {scnVerdict = Approved} | scnVerdict scenario /= Approved]
        <> [scenario {scnProvisional = False} | scnProvisional scenario]
        <> [scenario {scnExtraContributor = False} | scnExtraContributor scenario]
        <> [scenario {scnDebt = Nothing} | scnDebt scenario /= Nothing]
        <> [scenario {scnGateMode = mode} | mode <- [minBound .. scnGateMode scenario], mode /= scnGateMode scenario]
        <> [scenario {scnBlockingFinding = False} | scnBlockingFinding scenario]
        <> [scenario {scnResolveFinding = False} | scnResolveFinding scenario]
        <> [scenario {scnHeadMoved = False} | scnHeadMoved scenario]
        <> [scenario {scnPolicy = openPolicy} | scnPolicy scenario /= openPolicy]
        <> [scenario {scnTargetAfter = False} | scnTargetAfter scenario]
        <> [scenario {scnPolicyAfter = False} | scnPolicyAfter scenario]
        <> [scenario {scnAudit = Nothing} | scnAudit scenario /= Nothing]
        <> [scenario {scnEpisodeExpired = False} | scnEpisodeExpired scenario]
    referencedPatchsets current = case scnDebt current of
      Nothing -> []
      Just (index, _) -> [index]

-- | One invalidating transition applied to a valid history. Each mutation
-- removes exactly one load-bearing fact.
data Mutation = Mutation
  { mutationName :: String
  , mutationScenario :: Scenario
  }

-- | Whether the scenario's debt is bound to the newest patchset, where it
-- would rescue a rejected self-approval.
debtBindsLatest :: Scenario -> Bool
debtBindsLatest scenario = case scnDebt scenario of
  Just (index, _) -> index == scnPatchsets scenario
  Nothing -> False

mutations :: Scenario -> [Mutation]
mutations scenario =
  concat
    [ [Mutation "gate-omitted" scenario {scnGateMode = GateOmitted} | scnGateMode scenario == GateCovered]
    , [Mutation "gate-elsewhere" scenario {scnGateMode = GateOtherTree} | scnGateMode scenario == GateCovered]
    , [Mutation "gate-declaration-changed" scenario {scnGateMode = GateShapeMoved} | scnGateMode scenario == GateCovered]
    , [Mutation "gate-evidence-unreadable" scenario {scnGateMode = GateUnreadable} | scnGateMode scenario == GateCovered]
    , [Mutation "head-moved" scenario {scnHeadMoved = True} | not (scnHeadMoved scenario)]
    , [Mutation "target-moved" scenario {scnTargetAfter = True} | not (scnTargetAfter scenario)]
    , [Mutation "policy-moved" scenario {scnPolicyAfter = True} | not (scnPolicyAfter scenario)]
    , [Mutation "finding-opened" scenario {scnBlockingFinding = True} | scnReviewer scenario /= Nothing && not (scnBlockingFinding scenario)]
    , [Mutation "verdict-refused" scenario {scnVerdict = ChangesRequested} | scnReviewer scenario /= Nothing && scnVerdict scenario == Approved]
    , [Mutation "review-patchset-stale" scenario {scnVerdictOnFirst = True} | scnPatchsets scenario > 1 && scnReviewer scenario /= Nothing && not (debtBindsLatest scenario)]
    , [ Mutation "reviewer-is-contributor" scenario {scnReviewer = Just ActorContributor}
      | scnReviewer scenario == Just ActorIndependent
      , policyIndependentVerdictRequired (scnPolicy scenario)
      , policyForbidSelfApproval (scnPolicy scenario)
      , not (debtBindsLatest scenario)
      ]
    , [Mutation "waiver-expired" scenario {scnPatchsets = scnPatchsets scenario + 1} | scnDebt scenario /= Nothing && scnReviewer scenario == Nothing]
    ]

-- | The behaviours a generated scenario is expected to reach. A run that
-- never reaches one is reported so the suite cannot pass on trivial inputs.
data Feature
  = FeaturePermitted
  | FeatureWaived
  | FeatureSelfApprovalRefused
  | FeatureGatesRefused
  | FeatureVerdictStands
  | FeatureHeadMoved
  | FeatureTargetMoved
  | FeaturePolicyMoved
  | FeatureDebtUnused
  | FeatureUnknownObservation
  | FeatureAuditFulfilledRead
  | FeatureAuditNegativeNotApproved
  | FeatureEpisodeExpired
  | FeatureEqualTreeContributorVariation
  deriving (Eq, Ord, Show, Enum, Bounded)

allFeatures :: [Feature]
allFeatures = [minBound .. maxBound]

-- | Which of the required scenario classes a built scenario reached.
featureOf :: Scenario -> Built -> Feature -> Bool
featureOf scenario Built {..} feature = case feature of
  FeaturePermitted -> isPermitted builtDecision
  FeatureWaived -> case builtDecision of
    Permitted basis -> case basisAuthorization basis of
      AuthorizedByWaiver _ -> True
      AuthorizedByVerdictUnderWaiver _ _ -> True
      _ -> False
    Refused _ -> False
  FeatureSelfApprovalRefused -> refusedWith "self-approval"
  FeatureGatesRefused -> refusedWith "gates"
  FeatureVerdictStands -> refusedWith "verdict-stands"
  FeatureHeadMoved -> refusedWith "head-moved"
  FeatureTargetMoved -> movedFact isMovedTarget
  FeaturePolicyMoved -> movedFact isMovedPolicy
  FeatureDebtUnused -> case builtDecision of
    Permitted basis -> case basisAuthorization basis of
      AuthorizedByVerdict _ -> any ((== DebtId 1) . debtId) (stateDebts builtState)
      _ -> False
    Refused _ -> False
  FeatureUnknownObservation -> scnGateMode scenario `elem` [GateOmitted, GateUnreadable]
  FeatureAuditFulfilledRead -> coverageRead (coverageAfterIntegration builtFinalState) /= Nothing
  FeatureAuditNegativeNotApproved ->
    let coverage = coverageAfterIntegration builtFinalState
     in coverageVerdict coverage == Just ChangesRequested && not (coverageApproved coverage)
  FeatureEpisodeExpired -> any claimExpired (stateClaims builtFinalState)
  FeatureEqualTreeContributorVariation -> scnExtraContributor scenario
  where
    refusedWith tag = case builtDecision of
      Refused refusal -> refusalTag refusal == tag
      _ -> False
    movedFact predicate = case builtExecution of
      Left (RefusedBasisMoved facts) -> any predicate facts
      _ -> False
    isMovedTarget (MovedTarget _ _) = True
    isMovedTarget _ = False
    isMovedPolicy (MovedPolicy _ _) = True
    isMovedPolicy _ = False
