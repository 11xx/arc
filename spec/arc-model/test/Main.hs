{-# LANGUAGE RecordWildCards #-}

-- | The spec driver: unit fixtures, properties, mutant kills, and generator
-- coverage. Everything is deterministic from one seed.
module Main (main) where

import Data.List (intercalate)
import qualified Data.Set as Set
import System.Environment (getArgs)
import System.Exit (exitFailure, exitSuccess)
import Test.QuickCheck
import Test.QuickCheck.Gen (unGen)
import Test.QuickCheck.Random (mkQCGen)

import Arc.Model
import Fixtures (fixtureChecks)
import Generators
import Mutants
import Render

defaultSeed :: Int
defaultSeed = 20260907

defaultTests :: Int
defaultTests = 300

main :: IO ()
main = do
  args <- getArgs
  let seed = maybe defaultSeed read (optionValue "--seed" args)
      tests = maybe defaultTests read (optionValue "--tests" args)
  putStrLn ("arc-model spec: comparison revision " <> comparisonRevision)
  putStrLn ("seed " <> show seed <> ", " <> show tests <> " cases per property")
  putStrLn ""
  propertyResults <- propertyChecks seed tests
  mutantResults <- mutantChecks
  coverage <- coverageCheck seed tests
  let checks = fixtureChecks <> propertyResults <> mutantResults <> [coverage]
      failures = filter (not . checkPassed) checks
  mapM_ printCheck checks
  putStrLn ""
  putStrLn (show (length checks - length failures) <> "/" <> show (length checks) <> " checks passed")
  if null failures
    then do
      putStrLn "replay: cabal v2-test --test-options=\"--seed <seed> --tests <n>\""
      exitSuccess
    else do
      putStrLn "failed:"
      mapM_ (\check -> putStrLn ("  " <> checkName check <> ": " <> checkDetail check)) failures
      exitFailure

printCheck :: Check -> IO ()
printCheck Check {..} =
  putStrLn
    ( "["
        <> (if checkPassed then "ok" else "FAIL")
        <> "] "
        <> checkName
        <> (if null checkDetail then "" else "  " <> checkDetail)
    )

optionValue :: String -> [String] -> Maybe String
optionValue flag arguments = case dropWhile (/= flag) arguments of
  _ : value : _ -> Just value
  _ -> Nothing

-- | One QuickCheck run, deterministic from the seed.
runQC :: Int -> Int -> Property -> IO Result
runQC seed tests prop =
  quickCheckWithResult
    stdArgs
      { replay = Just (mkQCGen seed, 0)
      , maxSuccess = tests
      , chatty = False
      , maxDiscardRatio = 50
      }
    prop

runProperty :: String -> Int -> Int -> Property -> IO Check
runProperty name seed tests prop = do
  result <- runQC seed tests prop
  pure $
    if isSuccess result
      then passCheck name ""
      else failCheck name (counterexampleText (output result))

-- | The property suite.
propertyChecks :: Int -> Int -> IO [Check]
propertyChecks seed tests =
  sequence
    [ runProperty name (seed + index * 13) tests (forAllShrink generator shrinkScenario prop)
    | (index, (name, generator, prop)) <- zip [1 :: Int ..] properties
    ]

properties :: [(String, Gen Scenario, Scenario -> Property)]
properties =
  [ ("proposition: every integratable history permits", genIntegratable, prop_integratable_permitted)
  , ("proposition: one invalid transition flips the decision", genIntegratable, prop_mutation_flips)
  , ("proposition: every basis fact is grounded in the ledger", genAnyScenario, prop_basis_grounded)
  , ("proposition: an unknown observation never permits", genAnyScenario, prop_unknown_never_permits)
  , ("proposition: a moved basis stands down", genAnyScenario, prop_basis_moved_stands_down)
  , ("proposition: a waiver binds to exactly its patchset", genAnyScenario, prop_waiver_exact)
  , ("proposition: a refusing verdict is not waivable", genAnyScenario, prop_refusal_stands)
  , ("proposition: a fulfilled read needs an independent reader", genAnyScenario, prop_read_requires_independence)
  , ("proposition: a debt beside an approval authorized nothing", genAnyScenario, prop_debt_unused)
  , ("proposition: a negative audit that fulfils a read does not approve", genAnyScenario, prop_negative_audit_is_not_approval)
  ]

prop_integratable_permitted :: Scenario -> Property
prop_integratable_permitted scenario =
  isIntegratable scenario ==>
    counterexample (show scenario) (isPermitted (builtDecision (build scenario)))

prop_mutation_flips :: Scenario -> Property
prop_mutation_flips scenario =
  isIntegratable scenario ==>
    conjoin
      [ counterexample ("mutation " <> mutationName mutation) (mutationInvalidated (mutationScenario mutation))
      | mutation <- mutations scenario
      ]

-- | Either the mutated history refuses, or its execution stands down on a
-- moved basis.
mutationInvalidated :: Scenario -> Bool
mutationInvalidated scenario = case builtDecision built of
  Refused _ -> True
  Permitted _ -> case builtExecution built of
    Left (RefusedBasisMoved _) -> True
    _ -> False
  where
    built = build scenario

prop_basis_grounded :: Scenario -> Property
prop_basis_grounded scenario = case builtDecision built of
  Refused _ -> property True
  Permitted basis ->
    conjoin
      [ counterexample "the basis patchset is the latest patchset" (Just (basisPatchset basis) == (patchsetId <$> latestPatchset state))
      , counterexample "the basis tree is the evaluated tree" (basisTree basis == obsEvaluatedTree observation)
      , counterexample "the authorization is recorded" (authorizationGrounded state basis)
      , counterexample "every gate evaluation is recorded" (all (gateGrounded state (basisTree basis)) (basisGates basis))
      , counterexample "the consumed finding vector was empty" (null (basisConsumedFindings basis))
      , counterexample "the consumed hold vector was empty" (null (basisConsumedHolds basis))
      ]
  where
    built = build scenario
    state = builtState built
    observation = builtObservation built

authorizationGrounded :: ChangeState -> DecisionBasis -> Bool
authorizationGrounded state basis = case basisAuthorization basis of
  AuthorizedByVerdict event -> verdictGrounded state event patchset
  AuthorizedByWaiver debt -> debtGrounded state debt patchset
  AuthorizedByVerdictUnderWaiver event debt ->
    verdictGrounded state event patchset && debtGrounded state debt patchset
  where
    patchset = basisPatchset basis

verdictGrounded :: ChangeState -> EventId -> PatchsetId -> Bool
verdictGrounded state event patchset =
  any
    (\verdict -> verdictEvent verdict == event && verdictKind verdict == Approved && verdictPatchset verdict == patchset)
    (stateVerdicts state)

debtGrounded :: ChangeState -> DebtId -> PatchsetId -> Bool
debtGrounded state debt patchset =
  any (\record -> debtId record == debt && debtPatchset record == Just patchset) (stateDebts state)

gateGrounded :: ChangeState -> TreeId -> (GateName, EventId, DeclarationId) -> Bool
gateGrounded state tree (gate, event, declaration) =
  any
    ( \verification ->
        verificationEvent verification == event
          && verificationGate verification == gate
          && verificationDeclaration verification == declaration
          && verificationTree verification == tree
          && verificationResult verification == GatePass
          && verificationReadable verification
    )
    (stateVerifications state)

prop_unknown_never_permits :: Scenario -> Property
prop_unknown_never_permits scenario
  | scnGateMode scenario `elem` [GateOmitted, GateUnreadable, GateOtherTree, GateShapeMoved] =
      counterexample (show scenario) (not (isPermitted (builtDecision (build scenario))))
  | otherwise = property True

prop_basis_moved_stands_down :: Scenario -> Property
prop_basis_moved_stands_down scenario
  | scnTargetAfter scenario || scnPolicyAfter scenario = case builtDecision built of
      Permitted _ -> counterexample (show scenario) (isBasisMoved (builtExecution built))
      Refused _ -> property True
  | otherwise = property True
  where
    built = build scenario
    isBasisMoved (Left (RefusedBasisMoved _)) = True
    isBasisMoved _ = False

prop_waiver_exact :: Scenario -> Property
prop_waiver_exact scenario = case builtDecision built of
  Permitted basis -> case namedDebt (basisAuthorization basis) of
    Nothing -> property True
    Just debt ->
      counterexample (show scenario) $
        debtGrounded state debt (basisPatchset basis)
          && (debtId <$> newestWaiver state (basisPatchset basis)) == Just debt
  Refused _ -> property True
  where
    built = build scenario
    state = builtState built
    namedDebt authorization = case authorization of
      AuthorizedByWaiver debt -> Just debt
      AuthorizedByVerdictUnderWaiver _ debt -> Just debt
      AuthorizedByVerdict _ -> Nothing

prop_refusal_stands :: Scenario -> Property
prop_refusal_stands scenario =
  case (governingVerdict state, latestPatchset state) of
    (Just verdict, Just patchset)
      | verdictKind verdict `elem` [ChangesRequested, CommentOnly]
      , verdictPatchset verdict == patchsetId patchset ->
          counterexample (show scenario) (not (isPermitted (builtDecision built)))
    _ -> property True
  where
    built = build scenario
    state = builtState built

-- | A read is recorded only when somebody independent supplied one. The
-- projection never invents a reader, and a self-approval is not one.
prop_read_requires_independence :: Scenario -> Property
prop_read_requires_independence scenario =
  let state = builtFinalState (build scenario)
      coverage = coverageAfterIntegration state
   in counterexample (show scenario) (coverageRead coverage == Nothing || independentReadExists state)
  where
    independentReadExists state = case latestIntegration state of
      Nothing -> False
      Just record ->
        independentVerdictExists state record
          || any (\audit -> auditRevision audit == integratedHead record && auditIsIndependent state audit) (stateAudits state)
    independentVerdictExists state record = case integratedAuthorization record of
      AuthorizedByVerdict event -> case [v | v <- stateVerdicts state, verdictEvent v == event] of
        verdict : _ -> case patchsetById state (integratedPatchset record) of
          Just patchset ->
            not (verdictAssumed verdict)
              && verdictEffectiveActor verdict `Set.notMember` effectiveContributors patchset
          Nothing -> False
        [] -> False
      _ -> False

-- | The coverage obligation, the verdict outcome, and the historical
-- authorization are separate projections. A negative audit that fulfils the
-- read leaves the read fulfilled, the findings open, and the approval flag
-- false; the approval flag may only be true because some independent
-- approving answer exists.
prop_negative_audit_is_not_approval :: Scenario -> Property
prop_negative_audit_is_not_approval scenario =
  let final = builtFinalState (build scenario)
      coverage = coverageAfterIntegration final
   in counterexample (show scenario) $
        case (coverageRead coverage, coverageVerdict coverage, coverageAuthorization coverage) of
          (Just (ReadByAudit _), Just ChangesRequested, Just authorization)
            | AuthorizedByVerdict _ <- authorization -> property True
            | otherwise -> counterexample "a negative audit must not approve" (not (coverageApproved coverage))
          _ -> property True

prop_debt_unused :: Scenario -> Property
prop_debt_unused scenario = case builtDecision built of
  Permitted basis -> case basisAuthorization basis of
    AuthorizedByVerdict _ ->
      let bound = [debtId record | record <- stateDebts state, debtPatchset record == Just (basisPatchset basis)]
          unused = debtsNotUsed state (basisAuthorization basis)
       in counterexample (show scenario) (all (`elem` unused) bound)
    _ -> property True
  Refused _ -> property True
  where
    built = build scenario
    state = builtState built

-- | Coverage and historical mutants need a history that actually shipped,
-- so they draw from the integratable generator; the decision and execution
-- mutants need refusal scenarios, so they draw from the open one.
mutantGenerator :: Mutant -> Gen Scenario
mutantGenerator mutant = case mutantChannel mutant of
  ChannelCoverage -> genIntegratable
  ChannelHistorical -> genIntegratable
  _ -> genAnyScenario

-- | Every mutant must be killed, and killed for its predicted reason.
mutantChecks :: IO [Check]
mutantChecks = sequence [runMutantCheck index mutant | (index, mutant) <- zip [1 :: Int ..] allMutants]

runMutantCheck :: Int -> Mutant -> IO Check
runMutantCheck index mutant = do
  agreement <-
    runQC
      (1000 + index * 31)
      500
      (forAllShrink (mutantGenerator mutant) shrinkScenario (prop_mutant_agrees mutant))
  if isSuccess agreement
    then pure (failCheck ("mutant/" <> mutantName mutant) "SURVIVED: no generated scenario diverged")
    else do
      predicted <-
        runQC
          (2000 + index * 31)
          500
          (forAllShrink (mutantGenerator mutant) shrinkScenario (prop_mutant_predicted mutant))
      pure $
        if isSuccess predicted
          then passCheck ("mutant/" <> mutantName mutant) ("killed as " <> show (mutantPredicted mutant) <> ": " <> counterexampleText (output agreement))
          else failCheck ("mutant/" <> mutantName mutant) ("killed, but diverged for an unexpected reason: " <> counterexampleText (output predicted))

prop_mutant_agrees :: Mutant -> Scenario -> Property
prop_mutant_agrees mutant scenario =
  let built = build scenario
      expected = specBehaviour (mutantChannel mutant) built
      actual = mutantBehaviour mutant built
   in counterexample
        (show (divergenceOf actual expected) <> " on " <> counterexampleText (show scenario))
        (actual == expected)

prop_mutant_predicted :: Mutant -> Scenario -> Property
prop_mutant_predicted mutant scenario =
  let built = build scenario
   in counterexample
        (show (divergenceOf (mutantBehaviour mutant built) (specBehaviour (mutantChannel mutant) built)) <> " on " <> counterexampleText (show scenario))
        (predictedHolds (mutantPredicted mutant) (divergenceOf (mutantBehaviour mutant built) (specBehaviour (mutantChannel mutant) built)))

-- | Generate many scenarios and report which required classes were reached.
-- A run that never reaches one cannot claim the suite exercised it.
coverageCheck :: Int -> Int -> IO Check
coverageCheck seed tests = do
  let sampleCount = max 4000 tests
      samples =
        [ unGen genAnyScenario (mkQCGen (seed + index)) (index `mod` 40 + 1)
        | index <- [0 .. sampleCount - 1]
        ]
      built = [(scenario, build scenario) | scenario <- samples]
      counts =
        [ (feature, length [() | (scenario, value) <- built, featureOf scenario value feature])
        | feature <- allFeatures
        ]
      missing = [feature | (feature, count) <- counts, count == 0]
  pure $
    if null missing
      then passCheck "generator-coverage" (intercalate ", " [show feature <> "=" <> show count | (feature, count) <- counts])
      else failCheck "generator-coverage" ("features never reached: " <> intercalate ", " (map show missing))
