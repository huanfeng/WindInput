package dict

import "testing"

func TestPhraseLayerSearchCommandMarksIsCommand(t *testing.T) {
	pl := NewPhraseLayer("phrases", "")

	results := pl.SearchCommand("uuid", 10)
	if len(results) == 0 {
		t.Fatal("SearchCommand(uuid) should return candidates")
	}

	for i, c := range results {
		if !c.IsCommand {
			t.Fatalf("candidate[%d] should be marked IsCommand=true", i)
		}
	}
}

