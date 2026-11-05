package policy

import (
	"fmt"
	"path"
	"slices"
	"strings"
)

// HumanReview names the signals of a pull request that make a clean review of
// an auto-approving rule wait for the user. The engine evaluates them from
// GitHub data, never from reviewer output.
type HumanReview struct {
	// Labels hold the review when the pull request carries one of them, so an
	// author can ask for a human.
	Labels []string `json:"labels,omitempty"`
	// DeletedTests are path globs; deleting a file that matches one holds the
	// review, because a deleted test may be the only proof of a contract.
	DeletedTests []string `json:"deleted_tests,omitempty"`
}

type fileHumanReview struct {
	Labels       []string `yaml:"labels"`
	DeletedTests []string `yaml:"deleted_tests"`
}

const maxHumanReviewEntries = 50

func parseHumanReview(raw *fileHumanReview, onClean OnClean) (*HumanReview, error) {
	if raw == nil {
		return nil, nil
	}
	if onClean != CleanApprove {
		// Every other clean outcome already waits for a human or only comments.
		return nil, fmt.Errorf("human_review needs on_clean %s", CleanApprove)
	}
	if len(raw.Labels) == 0 && len(raw.DeletedTests) == 0 {
		return nil, fmt.Errorf("human_review must set labels or deleted_tests")
	}
	if len(raw.Labels) > maxHumanReviewEntries || len(raw.DeletedTests) > maxHumanReviewEntries {
		return nil, fmt.Errorf("human_review lists at most %d labels and %d deleted_tests globs", maxHumanReviewEntries, maxHumanReviewEntries)
	}
	labels := []string{}
	for _, label := range raw.Labels {
		if !labelPattern.MatchString(label) || slices.ContainsFunc(labels, func(other string) bool { return strings.EqualFold(other, label) }) {
			return nil, fmt.Errorf("human_review labels: %q must be a valid label listed once", label)
		}
		labels = append(labels, label)
	}
	globs := []string{}
	for _, glob := range raw.DeletedTests {
		if err := validGlob(glob); err != nil || slices.Contains(globs, glob) {
			return nil, fmt.Errorf("human_review deleted_tests: %q must be a valid relative glob listed once", glob)
		}
		globs = append(globs, glob)
	}
	return &HumanReview{Labels: labels, DeletedTests: globs}, nil
}

// validGlob accepts a slash-separated relative pattern whose segments are
// path.Match patterns or "**".
func validGlob(glob string) error {
	if glob == "" || len(glob) > 256 || strings.HasPrefix(glob, "/") || strings.Contains(glob, "\\") {
		return fmt.Errorf("invalid glob")
	}
	for _, segment := range strings.Split(glob, "/") {
		if segment == "" || segment == "." || segment == ".." {
			return fmt.Errorf("invalid glob")
		}
		if segment == "**" {
			continue
		}
		if strings.Contains(segment, "**") {
			// "**" only spans directories as a whole segment.
			return fmt.Errorf("invalid glob")
		}
		if _, err := path.Match(segment, ""); err != nil {
			return err
		}
	}
	return nil
}

// MatchGlob reports whether a repository path matches a glob of validGlob:
// "*", "?" and classes match within one segment, "**" matches any number of
// segments, including none.
func MatchGlob(glob, name string) bool {
	return matchSegments(strings.Split(glob, "/"), strings.Split(name, "/"))
}

func matchSegments(pattern, name []string) bool {
	for len(pattern) > 0 {
		if pattern[0] == "**" {
			for skip := 0; skip <= len(name); skip++ {
				if matchSegments(pattern[1:], name[skip:]) {
					return true
				}
			}
			return false
		}
		if len(name) == 0 {
			return false
		}
		if ok, err := path.Match(pattern[0], name[0]); err != nil || !ok {
			return false
		}
		pattern, name = pattern[1:], name[1:]
	}
	return len(name) == 0
}
