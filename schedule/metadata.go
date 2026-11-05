package schedule

import (
	"fmt"
	"regexp"
	"strings"
)

var headPattern = regexp.MustCompile(`^[0-9a-f]{40}$`)

// WorkSession identifies the pull request a review session belongs to. The
// keys follow the pohunek-work launch contract, so its list joins the session
// to the pull request row (work.link.id is "owner/name#number").
type WorkSession struct {
	Repository string
	Number     int
	URL        string
	HeadBranch string
	HeadSHA    string
}

var (
	metadataRepositoryPattern = regexp.MustCompile(`^[a-z0-9_.-]+/[a-z0-9_.-]+$`)
	metadataBranchPattern     = regexp.MustCompile(`^[A-Za-z0-9_][A-Za-z0-9_./-]{0,254}$`)
	// SessionGroupPattern bounds the companion group name to what
	// pohunek-companion accepts at its default 64-byte limit: no control or
	// format characters and nothing it would trim away.
	SessionGroupPattern = regexp.MustCompile(`^[A-Za-z0-9](?:[A-Za-z0-9 ._-]{0,62}[A-Za-z0-9._-])?$`)
)

// MetadataArgs returns `session new --meta` arguments in a fixed order. A head
// branch that is not a plain ref name is omitted; the link id alone joins the row.
// The group is written to companion.group, the key pohunek-companion places a
// session in a sidebar group by.
func MetadataArgs(session WorkSession, group string) ([]string, error) {
	if !SessionGroupPattern.MatchString(group) {
		return nil, fmt.Errorf("invalid review session group")
	}
	if !metadataRepositoryPattern.MatchString(session.Repository) || session.Number <= 0 ||
		!headPattern.MatchString(session.HeadSHA) ||
		!strings.HasPrefix(session.URL, fmt.Sprintf("https://github.com/%s/pull/", session.Repository)) ||
		strings.ContainsAny(session.URL, " \t\r\n\x00") {
		return nil, fmt.Errorf("invalid review session identity")
	}
	entries := [][2]string{
		{"work.link.provider", "github"},
		{"work.link.kind", "pull_request"},
		{"work.link.id", fmt.Sprintf("%s#%d", session.Repository, session.Number)},
		{"work.link.url", session.URL},
	}
	if metadataBranchPattern.MatchString(session.HeadBranch) && !strings.Contains(session.HeadBranch, "..") {
		entries = append(entries, [2]string{"work.link.branch", session.HeadBranch})
	}
	entries = append(entries, [2]string{"work.role", "review"}, [2]string{"work.rev", session.HeadSHA}, [2]string{"companion.group", group})
	args := make([]string, 0, 2*len(entries))
	for _, entry := range entries {
		args = append(args, "--meta", entry[0]+"="+entry[1])
	}
	return args, nil
}
