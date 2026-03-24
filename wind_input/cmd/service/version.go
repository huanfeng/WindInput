package main

// 版本信息，通过 ldflags 在构建时注入：
//
//	go build -ldflags "-X main.version=0.1.0-alpha"
var version = "dev"
