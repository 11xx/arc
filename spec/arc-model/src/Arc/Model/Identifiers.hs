{-# LANGUAGE GeneralizedNewtypeDeriving #-}

-- | Distinct identifier types.
--
-- Every coordinate the model compares is its own type. A revision is not a
-- tree, a patchset is not a change, and a gate is not a declaration, so a
-- mix-up is a type error rather than a comparison that happens to succeed.
module Arc.Model.Identifiers
  ( ChangeId (..)
  , PatchsetId (..)
  , Revision (..)
  , TreeId (..)
  , ActorId (..)
  , GateName (..)
  , DeclarationId (..)
  , EventId (..)
  , FindingId (..)
  , DebtId (..)
  , ClaimId (..)
  , HoldId (..)
  , FailureLabel (..)
  , TargetBranch (..)
  ) where

import Data.String (IsString)

newtype ChangeId = ChangeId String
  deriving (Eq, Ord, Show, IsString)

newtype PatchsetId = PatchsetId Int
  deriving (Eq, Ord, Show, Enum)

newtype Revision = Revision String
  deriving (Eq, Ord, Show, IsString)

newtype TreeId = TreeId String
  deriving (Eq, Ord, Show, IsString)

newtype ActorId = ActorId String
  deriving (Eq, Ord, Show, IsString)

newtype GateName = GateName String
  deriving (Eq, Ord, Show, IsString)

newtype DeclarationId = DeclarationId String
  deriving (Eq, Ord, Show, IsString)

newtype EventId = EventId Int
  deriving (Eq, Ord, Show, Enum)

newtype FindingId = FindingId Int
  deriving (Eq, Ord, Show, Enum)

newtype DebtId = DebtId Int
  deriving (Eq, Ord, Show, Enum)

newtype ClaimId = ClaimId Int
  deriving (Eq, Ord, Show, Enum)

newtype HoldId = HoldId Int
  deriving (Eq, Ord, Show, Enum)

newtype FailureLabel = FailureLabel String
  deriving (Eq, Ord, Show, IsString)

newtype TargetBranch = TargetBranch String
  deriving (Eq, Ord, Show, IsString)
