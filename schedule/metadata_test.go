package schedule

import (
	"strings"
	"testing"
)

func TestMetadataArgsFollowThePohunekWorkLaunchContract(t *testing.T) {
	head := strings.Repeat("a", 40)
	args, err := MetadataArgs(WorkSession{Repository: "example-org/example-repo", Number: 8605, URL: "https://github.com/example-org/example-repo/pull/8605", HeadBranch: "example-user/ABC-1/fix", HeadSHA: head}, "autoreview")
	if err != nil {
		t.Fatal(err)
	}
	want := []string{
		"--meta", "work.link.provider=github",
		"--meta", "work.link.kind=pull_request",
		"--meta", "work.link.id=example-org/example-repo#8605",
		"--meta", "work.link.url=https://github.com/example-org/example-repo/pull/8605",
		"--meta", "work.link.branch=example-user/ABC-1/fix",
		"--meta", "work.role=review",
		"--meta", "work.rev=" + head,
		"--meta", "companion.group=autoreview",
	}
	if strings.Join(args, "\n") != strings.Join(want, "\n") {
		t.Fatalf("args = %q", args)
	}
}

func TestMetadataArgsOmitUnsafeBranchAndRejectInvalidIdentity(t *testing.T) {
	head := strings.Repeat("a", 40)
	valid := WorkSession{Repository: "example-org/example-repo", Number: 1, URL: "https://github.com/example-org/example-repo/pull/1", HeadSHA: head}
	for _, branch := range []string{"", "-option", "a..b", "with space"} {
		session := valid
		session.HeadBranch = branch
		args, err := MetadataArgs(session, "autoreview")
		if err != nil || strings.Contains(strings.Join(args, " "), "work.link.branch") {
			t.Fatalf("branch %q: args=%q err=%v", branch, args, err)
		}
	}
	for _, mutate := range []func(*WorkSession){
		func(s *WorkSession) { s.Repository = "Example-Org/Example-Repo" },
		func(s *WorkSession) { s.Number = 0 },
		func(s *WorkSession) { s.HeadSHA = "abc" },
		func(s *WorkSession) { s.URL = "https://github.com/other/repo/pull/1" },
		func(s *WorkSession) { s.URL += "\n" },
	} {
		session := valid
		mutate(&session)
		if _, err := MetadataArgs(session, "autoreview"); err == nil {
			t.Fatalf("accepted invalid identity %#v", session)
		}
	}
}

func TestMetadataArgsRejectAGroupTheCompanionWouldNotShow(t *testing.T) {
	session := WorkSession{Repository: "example-org/example-repo", Number: 1, URL: "https://github.com/example-org/example-repo/pull/1", HeadSHA: strings.Repeat("a", 40)}
	for _, group := range []string{"", " autoreview", "autoreview ", "auto\nreview", "auto\u202ereview", "auto\u200breview", strings.Repeat("a", 65)} {
		if _, err := MetadataArgs(session, group); err == nil {
			t.Fatalf("accepted group %q", group)
		}
	}
	for _, group := range []string{"autoreview", "Auto review", strings.Repeat("a", 64)} {
		if _, err := MetadataArgs(session, group); err != nil {
			t.Fatalf("group %q: %v", group, err)
		}
	}
}
