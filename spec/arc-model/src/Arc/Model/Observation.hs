{-# LANGUAGE RecordWildCards #-}

-- | Observations: what the model was told, and how much of it it actually
-- saw.
--
-- A fact that was not observed is 'Omitted'. It is not false, not passed, and
-- not failed: the model never promotes a missing observation into a result.
-- Pass/fail, coverage, availability, and demonstrated falsification are four
-- separate readings of the same gate, kept apart because they answer four
-- different questions.
module Arc.Model.Observation
  ( Observed (..)
  , observedToMaybe
  , GateResult (..)
  , ExecutionKind (..)
  , Declaration (..)
  , DeclarationShape (..)
  , declarationShape
  , Verification (..)
  , GateCoverage (..)
  , EvidenceAvailability (..)
  , GateReading (..)
  , GateRefusal (..)
  , gateRefusalText
  , readGate
  , gateGreen
  , Policy (..)
  , Observations (..)
  ) where

import Arc.Model.Identifiers

-- | A fact the model may or may not have been told.
data Observed a = Omitted | Observed a
  deriving (Eq, Ord, Show)

observedToMaybe :: Observed a -> Maybe a
observedToMaybe Omitted = Nothing
observedToMaybe (Observed a) = Just a

-- | Whether a gate command reported success or failure when it ran.
data GateResult = GatePass | GateFail
  deriving (Eq, Ord, Show)

-- | How a record came to exist. Arc running the command itself is not the
-- same claim as somebody attesting that it ran.
data ExecutionKind = RanLocally | Attested
  deriving (Eq, Ord, Show)

-- | A required check, declared with the exact command and timeout it will be
-- recognized by. Evidence is green for a declaration, and a changed
-- declaration is a check that has not run.
data Declaration = Declaration
  { declarationId :: DeclarationId
  , declarationCommand :: String
  , declarationTimeoutSeconds :: Int
  }
  deriving (Eq, Ord, Show)

data DeclarationShape = DeclarationShape
  { shapeCommand :: String
  , shapeTimeoutSeconds :: Int
  }
  deriving (Eq, Ord, Show)

declarationShape :: Declaration -> DeclarationShape
declarationShape Declaration {..} =
  DeclarationShape
    { shapeCommand = declarationCommand
    , shapeTimeoutSeconds = declarationTimeoutSeconds
    }

-- | One recorded gate evaluation. 'verificationAnswers' names the failure a
-- passing run was observed to answer, which is what separates a gate shown
-- able to fail from one that has only ever passed. That distinction is
-- advisory: it never blocks a merge.
data Verification = Verification
  { verificationEvent :: EventId
  , verificationGate :: GateName
  , verificationDeclaration :: DeclarationId
  , verificationShape :: DeclarationShape
  , verificationTree :: TreeId
  , verificationResult :: GateResult
  , verificationExecution :: ExecutionKind
  , verificationAnswers :: Maybe FailureLabel
  , verificationReadable :: Bool
  }
  deriving (Eq, Ord, Show)

-- | Whether the required declaration was evaluated, and against which tree.
data GateCoverage
  = Covered EventId
  | NeverEvaluated
  | EvaluatedOtherTree TreeId
  | DeclarationMoved DeclarationId
  deriving (Eq, Ord, Show)

-- | Whether evidence exists at all, and whether the record could be read.
data EvidenceAvailability
  = NotProduced
  | Recorded ExecutionKind
  | EvidenceUnreadable
  deriving (Eq, Ord, Show)

-- | Four independent readings of one required gate.
data GateReading = GateReading
  { readingGate :: GateName
  , readingResult :: Observed GateResult
  -- ^ The result last observed for this declaration, wherever it ran.
  , readingCoverage :: GateCoverage
  -- ^ Whether that observation answers the declaration and tree in force.
  , readingAvailability :: EvidenceAvailability
  -- ^ Whether any record exists, and whether it could be read.
  , readingFalsified :: Observed FailureLabel
  -- ^ The failure this gate was demonstrated to answer, when it was.
  }
  deriving (Eq, Ord, Show)

-- | Why a required gate does not count as green.
data GateRefusal
  = GateNotDeclared GateName
  | GateNeverEvaluated GateName
  | GateEvaluatedOtherTree GateName TreeId
  | GateDeclarationChanged GateName
  | GateFailed GateName EventId
  | GateEvidenceUnreadable GateName
  deriving (Eq, Ord, Show)

gateRefusalText :: GateRefusal -> String
gateRefusalText refusal = case refusal of
  GateNotDeclared (GateName name) -> "gate " <> name <> " is required but not declared"
  GateNeverEvaluated (GateName name) -> "gate " <> name <> " has never been evaluated"
  GateEvaluatedOtherTree (GateName name) (TreeId tree) ->
    "gate " <> name <> " was evaluated at tree " <> tree <> ", not the evaluated tree"
  GateDeclarationChanged (GateName name) ->
    "gate " <> name <> " declaration changed; the declared check has not run"
  GateFailed (GateName name) event ->
    "gate " <> name <> " failed at event " <> show event
  GateEvidenceUnreadable (GateName name) ->
    "gate " <> name <> " evidence could not be read; that is not a result"

-- | Read one required gate from the recorded verifications. Coverage and
-- availability are decided by the newest record, so an unreadable newest
-- record leaves the gate refused rather than falling back to an older pass;
-- any older result is reported beside them as a result, never as coverage.
readGate :: GateName -> Declaration -> TreeId -> [Verification] -> GateReading
readGate gate Declaration {..} tree verifications = GateReading {..}
  where
    matching =
      [ v
      | v <- verifications
      , verificationGate v == gate
      , verificationDeclaration v == declarationId
      ]
    shape = declarationShape (Declaration {..})
    newest = lastMaybe matching
    newestReadable = lastMaybe (filter verificationReadable matching)
    atTree = filter ((== tree) . verificationTree) (filter ((== shape) . verificationShape) matching)
    readingGate = gate
    readingResult = maybe Omitted (Observed . verificationResult) newestReadable
    readingCoverage = case newest of
      Nothing -> NeverEvaluated
      Just v
        | not (verificationReadable v) -> NeverEvaluated
        | verificationShape v /= shape -> DeclarationMoved declarationId
        | verificationTree v == tree -> Covered (verificationEvent v)
        | otherwise -> EvaluatedOtherTree (verificationTree v)
    readingAvailability = case newest of
      Nothing -> NotProduced
      Just v
        | not (verificationReadable v) -> EvidenceUnreadable
        | otherwise -> Recorded (verificationExecution v)
    readingFalsified = case lastMaybe atTree of
      Just v -> maybe Omitted Observed (verificationAnswers v)
      Nothing -> Omitted
    lastMaybe [] = Nothing
    lastMaybe xs = Just (last xs)

-- | Whether a required gate counts as green, and why not when it does not.
-- The first question is coverage, so a pass recorded elsewhere never stands
-- in for the declaration and tree in force.
gateGreen :: GateName -> Maybe Declaration -> TreeId -> [Verification] -> Either GateRefusal GateReading
gateGreen gate Nothing _ _ = Left (GateNotDeclared gate)
gateGreen gate (Just declaration) tree verifications =
  let reading = readGate gate declaration tree verifications
   in case readingCoverage reading of
        Covered event -> case readingResult reading of
          Observed GatePass -> Right reading
          Observed GateFail -> Left (GateFailed gate event)
          Omitted -> Left (GateEvidenceUnreadable gate)
        NeverEvaluated -> case readingAvailability reading of
          EvidenceUnreadable -> Left (GateEvidenceUnreadable gate)
          _ -> Left (GateNeverEvaluated gate)
        EvaluatedOtherTree other -> Left (GateEvaluatedOtherTree gate other)
        DeclarationMoved _ -> Left (GateDeclarationChanged gate)

-- | The repository's declared integration policy, as observed at decision
-- time.
data Policy = Policy
  { policyIndependentVerdictRequired :: Bool
  -- ^ The change touches a surface where a verdict must come from somebody
  -- other than its author.
  , policyForbidSelfApproval :: Bool
  -- ^ A self-recorded approval is rejected rather than accepted.
  , policyRequireDeclaredActor :: Bool
  -- ^ An event whose author nobody claimed is refused.
  }
  deriving (Eq, Ord, Show)

-- | Everything the model is told about the world at decision time. The
-- evaluated tree is explicit because a change behind its target evaluates
-- the merge, not its own head, and only Git can say what that tree is.
data Observations = Observations
  { obsChange :: ChangeId
  , obsHead :: Revision
  , obsTargetBranch :: TargetBranch
  , obsTarget :: Revision
  , obsEvaluatedTree :: TreeId
  , obsDeclarations :: [Declaration]
  , obsRequiredGates :: [(GateName, DeclarationId)]
  , obsPolicy :: Policy
  , obsBlockedBy :: [ChangeId]
  , obsInvokerDeclared :: Bool
  }
  deriving (Eq, Ord, Show)
