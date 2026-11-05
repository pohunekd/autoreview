package schedule

import (
	"encoding/json"
	"fmt"
	"strconv"
	"time"
)

func LoadPolicy(lookup func(string) (string, bool), prefix string) (Policy, error) {
	debounceName := prefix + "_DEBOUNCE_SECONDS"
	debounce, _ := lookup(debounceName)
	seconds, err := strconv.Atoi(debounce)
	if err != nil || seconds <= 0 || seconds > 600 {
		return Policy{}, fmt.Errorf("invalid or missing configuration: %s", debounceName)
	}
	delaysName := prefix + "_RETRY_DELAYS_SECONDS_JSON"
	raw, _ := lookup(delaysName)
	var values []int
	if err := json.Unmarshal([]byte(raw), &values); err != nil || len(values) == 0 || len(values) > 5 {
		return Policy{}, fmt.Errorf("invalid or missing configuration: %s", delaysName)
	}
	p := Policy{Debounce: time.Duration(seconds) * time.Second}
	previous := 0
	for _, value := range values {
		if value <= previous || value > 3600 {
			return Policy{}, fmt.Errorf("invalid retry delay in %s", delaysName)
		}
		p.RetryDelays = append(p.RetryDelays, time.Duration(value)*time.Second)
		previous = value
	}
	return p, nil
}
