class Account:
    owner_id: int = 0

    def load(self, owner_id: int) -> None:
        self.owner_id = owner_id
