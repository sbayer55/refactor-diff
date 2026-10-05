class Account:
    owner_id: str = 0

    def load(self, owner_id: str) -> None:
        self.owner_id = owner_id
