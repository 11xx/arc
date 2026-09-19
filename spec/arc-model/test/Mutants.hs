{-# LANGUAGE RecordWildCards #-}

-- | Deliberate faults.
--
-- Each mutant changes exactly one rule of the model. The suite must kill
-- every one of them, and it must kill each for the predicted reason: a
-- mutant that permits where the model refuses is a different fault from one
-- that rewrites a recorded basis. A mutant that survives is a gap in the
-- suite, not a hobby.
module Mutants
  ( Behaviour (..)
  , Channel (..)
  , specBehaviour
  , Predicted
  , Divergence (..)
  , divergenceOf
  , predictedHolds
  , Mutant (..)
  , mutantBehaviour
  , allMutants
  ) where

import Arc.Model
import Generators

-- | One observable channel of the model.
data Behaviour
  = BehaviourDecision Decision
  | BehaviourExecution (Either Refusal ExecutionPlan)
  | BehaviourHistorical (Maybe Authorization)
  | BehaviourCoverage CoverageAfterIntegration
  deriving (Eq, Show)

data Channel = ChannelDecision | ChannelExecution | ChannelHistorical | ChannelCoverage
  deriving (Eq, Show)

-- | What the model says on one channel.
specBehaviour :: Channel -> Built -> Behaviour
specBehaviour channel Built {..} = case channel of
  ChannelDecision -> BehaviourDecision builtDecision
  ChannelExecution -> BehaviourExecution builtExecution
  ChannelHistorical -> BehaviourHistorical (historicalAuthorization builtFinalState)
  ChannelCoverage -> BehaviourCoverage (coverageAfterIntegration builtFinalState)

-- | The divergence classes a mutant is allowed to exhibit. A fault can show
-- as a permission where the model refused, or as one refusal replaced by
-- another that no longer rests on the dropped fact.
type Predicted = [Divergence]

-- | How a mutant's answer differs from the model's.
data Divergence
  = DivergenceAgrees
  | DivergencePermits
  | DivergenceRefuses
  | DivergenceDifferentRefusal
  | DivergenceDifferentValue
  deriving (Eq, Show)

divergenceOf :: Behaviour -> Behaviour -> Divergence
divergenceOf mutant spec
  | mutant == spec = DivergenceAgrees
  | otherwise = case (mutant, spec) of
      (BehaviourDecision (Permitted _), BehaviourDecision (Refused _)) -> DivergencePermits
      (BehaviourDecision (Refused _), BehaviourDecision (Permitted _)) -> DivergenceRefuses
      (BehaviourDecision (Refused _), BehaviourDecision (Refused _)) -> DivergenceDifferentRefusal
      (BehaviourExecution (Right _), BehaviourExecution (Left _)) -> DivergencePermits
      (BehaviourExecution (Left _), BehaviourExecution (Right _)) -> DivergenceRefuses
      (BehaviourExecution (Left _), BehaviourExecution (Left _)) -> DivergenceDifferentRefusal
      _ -> DivergenceDifferentValue

predictedHolds :: Predicted -> Divergence -> Bool
predictedHolds predicted divergence =
  divergence == DivergenceAgrees || divergence `elem` predicted

data Mutant = Mutant
  { mutantName :: String
  , mutantChannel :: Channel
  , mutantPredicted :: Predicted
  , mutantRun :: Built -> Behaviour
  }

mutantBehaviour :: Mutant -> Built -> Behaviour
mutantBehaviour Mutant {..} = mutantRun

allMutants :: [Mutant]
allMutants =
  [ Mutant
      { mutantName = "contributor-identity-ignored"
      , mutantChannel = ChannelDecision
      , mutantPredicted = [DivergencePermits, DivergenceDifferentRefusal, DivergenceDifferentValue]
      , mutantRun = \b -> BehaviourDecision (decide (builtObservation b) (withoutContributors (builtState b)))
      }
  , Mutant
      { mutantName = "gate-matched-by-name"
      , mutantChannel = ChannelDecision
      , mutantPredicted = [DivergencePermits, DivergenceDifferentRefusal]
      , mutantRun = \b -> BehaviourDecision (decide (builtObservation b) (evidenceNormalized b))
      }
  , Mutant
      { mutantName = "unknown-treated-as-success"
      , mutantChannel = ChannelDecision
      , mutantPredicted = [DivergencePermits, DivergenceDifferentRefusal]
      , mutantRun = \b -> BehaviourDecision (decide (builtObservation b) (evidenceFabricated b))
      }
  , Mutant
      { mutantName = "authorization-reused-after-basis-moved"
      , mutantChannel = ChannelExecution
      , mutantPredicted = [DivergencePermits, DivergenceDifferentRefusal]
      , mutantRun = \b -> BehaviourExecution (execute (builtObservation b) (builtState b) (builtDecision b))
      }
  , Mutant
      { mutantName = "fulfilled-implies-approved"
      , mutantChannel = ChannelCoverage
      , mutantPredicted = [DivergenceDifferentValue]
      , mutantRun = \b ->
          let coverage = coverageAfterIntegration (builtFinalState b)
           in BehaviourCoverage coverage {coverageApproved = coverageApproved coverage || coverageRead coverage /= Nothing}
      }
  , Mutant
      { mutantName = "latest-debt-applied-to-every-patchset"
      , mutantChannel = ChannelDecision
      , mutantPredicted = [DivergencePermits, DivergenceDifferentRefusal]
      , mutantRun = \b -> BehaviourDecision (decide (builtObservation b) (debtsRelocated b))
      }
  , Mutant
      { mutantName = "debt-clears-refusing-verdict"
      , mutantChannel = ChannelDecision
      , mutantPredicted = [DivergencePermits, DivergenceDifferentRefusal]
      , mutantRun = \b -> BehaviourDecision (decide (builtObservation b) (refusalDropped b))
      }
  , Mutant
      { mutantName = "later-audit-rewrites-integration-basis"
      , mutantChannel = ChannelHistorical
      , mutantPredicted = [DivergenceDifferentValue]
      , mutantRun = \b ->
          let audits = stateAudits (builtFinalState b)
           in BehaviourHistorical (case audits of
                    [] -> historicalAuthorization (builtFinalState b)
                    _ -> Just (AuthorizedByVerdict (auditEvent (last audits)))
                  )
      }
  , Mutant
      { mutantName = "unreadable-evidence-counts-as-review"
      , mutantChannel = ChannelDecision
      , mutantPredicted = [DivergencePermits, DivergenceDifferentRefusal]
      , mutantRun = \b -> BehaviourDecision (decide (builtObservation b) (evidenceMadeReadable b))
      }
  ]

-- | A state where the reviewer's identity is not compared against the
-- contributor set: every verdict reads as if it came from a declared
-- stranger. The assumed-identity half of the rule still applies, because a
-- mutant that ignores identity entirely would not be the fault under test.
withoutContributors :: ChangeState -> ChangeState
withoutContributors state =
  state
    { stateVerdicts =
        [ verdict
            { verdictActor = ActorId "identity-ignored"
            , verdictOnBehalfOf = Nothing
            }
        | verdict <- stateVerdicts state
        ]
    }

-- | A state where every recorded evaluation is moved to the evaluated tree
-- and the declared shape, so a gate is green by name alone.
evidenceNormalized :: Built -> ChangeState
evidenceNormalized Built {..} =
  builtState
    { stateVerifications = map normalize (stateVerifications builtState)
    }
  where
    observation = builtObservation
    normalize verification =
      case declarationFor (verificationDeclaration verification) of
        Just declaration ->
          verification
            { verificationTree = obsEvaluatedTree observation
            , verificationShape = declarationShape declaration
            }
        Nothing -> verification
    declarationFor wanted = case [d | d <- obsDeclarations observation, declarationId d == wanted] of
      declaration : _ -> Just declaration
      [] -> Nothing

-- | A state where a missing observation is answered with a passing record.
evidenceFabricated :: Built -> ChangeState
evidenceFabricated built = case builtDecision built of
  Refused (RefusedGates _) ->
    (builtState built) {stateVerifications = stateVerifications (builtState built) <> fabricated}
  _ -> builtState built
  where
    observation = builtObservation built
    fabricated = case obsRequiredGates observation of
      [] -> []
      (gate, wanted) : _ ->
        case [d | d <- obsDeclarations observation, declarationId d == wanted] of
          [] -> []
          declaration : _ ->
            [ Verification
                { verificationEvent = EventId 990
                , verificationGate = gate
                , verificationDeclaration = declarationId declaration
                , verificationShape = declarationShape declaration
                , verificationTree = obsEvaluatedTree observation
                , verificationResult = GatePass
                , verificationExecution = RanLocally
                , verificationAnswers = Nothing
                , verificationReadable = True
                }
            ]

-- | A state where one debt declaration is rebound from its patchset to the
-- newest one.
debtsRelocated :: Built -> ChangeState
debtsRelocated Built {..} =
  builtState
    { stateDebts = [debt {debtPatchset = latest} | debt <- stateDebts builtState]
    }
  where
    latest = patchsetId <$> latestPatchset builtState

-- | A state where a refusing verdict has been dropped, as if the debt
-- cleared it.
refusalDropped :: Built -> ChangeState
refusalDropped built = case builtDecision built of
  Refused (RefusedVerdictStands _ event)
    | waiverBindsLatest -> (builtState built) {stateVerdicts = filter ((/= event) . verdictEvent) (stateVerdicts (builtState built))}
  _ -> builtState built
  where
    waiverBindsLatest = case latestPatchset (builtState built) of
      Nothing -> False
      Just patchset -> not (null (debtsForPatchset (builtState built) (patchsetId patchset)))

-- | A state where every evidence record is readable.
evidenceMadeReadable :: Built -> ChangeState
evidenceMadeReadable Built {..} =
  builtState
    { stateVerifications = [verification {verificationReadable = True} | verification <- stateVerifications builtState]
    }
